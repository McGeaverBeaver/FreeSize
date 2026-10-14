//! Application state and the logic behind every UI callback.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{MAIN_SEPARATOR_STR, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use freesize_core::bubbles::{self, BubbleOptions};
use freesize_core::ncdu;
use freesize_core::snapshot::{self, Snapshot};
use freesize_core::treemap::{self, Rect, TreemapOptions};
use freesize_core::{
    Bin, DuplicateGroup, DuplicateProgress, Kind, NO_NODE, NodeId, ScanOptions, ScanProgress,
    ScanResult, SizeMode, Spot, Tree, age_bins, default_spot_rules, disk_space,
    duplicate_candidates, extension_stats, format_count, format_size, has_extension, kinds,
    largest_files, largest_files_where, rebuildable, scan, size_bins, system_spots,
};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel,
};

use crate::appbubbles;
use crate::apps::{self, AppReport};
use crate::changes;
use crate::cleanup::{self, JunkKind, Place, ReviewKind};
use crate::elevate;
use crate::filecleanup::{self, Duplicates, Hidden, Similar};
use crate::growth::{self, DriveLog};
use crate::hidden;
use crate::home::{self, AppsState, Section, sheet};
use crate::hub::{self, Hub};
use crate::insights::{self, View};
use crate::junk;
use crate::moves;
use crate::owners::{self, Owners};
use crate::quarantine;
use crate::report;
use crate::review;
use crate::rows::{self, TreeRows};
use crate::scans;
use crate::similar;
use crate::updater::{self, Updater};
use crate::{
    AppWindow, BasketRow, BubbleItem, CleanRow, CleanSection, Crumb, FileRow, HomeRow, JunkRow,
    MapRect, NavItem, PhotoSet, SheetAction, TypeRow, actions, locations,
};

/// The Export sheet's buttons.
const EXPORT_HTML: &str = "export-html";
const EXPORT_CSV: &str = "export-csv";
const EXPORT_JSON: &str = "export-json";

/// A saved scan to open rather than a folder to scan.
fn is_scan_file(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        && path.is_file()
}

/// Icons beyond the kinds of content (see `Icons.of` in the UI).
const ICON_REBUILD: i32 = 12;
const ICON_MORE: i32 = 13;
/// Most files listed when the list is limited to one group.
const MAX_OF_TYPE: usize = 1000;
/// Keys of the Types tab's rows: a type by extension, an age or size group
/// by position.
const TYPE_KEY: &str = "type:";
const AGE_KEY: &str = "age:";
const SIZE_KEY: &str = "size:";

/// What the files list is limited to.
#[derive(Clone, Debug, PartialEq)]
enum FileFilter {
    Type(String),
    Modified(Bin),
    Size(Bin),
    /// Files with a date before this, in seconds since 1970.
    OlderThan(i64),
}

impl FileFilter {
    fn matches(&self, tree: &Tree, id: NodeId, mode: SizeMode) -> bool {
        let node = tree.node(id);
        match self {
            FileFilter::Type(ext) => has_extension(tree, id, ext),
            FileFilter::Modified(bin) => bin.from <= node.modified && node.modified < bin.to,
            FileFilter::Size(bin) => {
                let size = i64::try_from(node.size_in(mode)).unwrap_or(i64::MAX);
                bin.from <= size && size < bin.to
            }
            FileFilter::OlderThan(time) => node.modified > 0 && node.modified < *time,
        }
    }
}

/// Result handed from a worker thread to the UI thread.
type Slot<T> = Arc<Mutex<Option<T>>>;

fn take<T>(slot: &Slot<T>) -> Option<T> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

fn put<T>(slot: &Slot<T>, value: T) {
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
}

struct RunningScan {
    path: PathBuf,
    progress: Arc<ScanProgress>,
    result: Slot<io::Result<Scanned>>,
}

/// A finished scan with what's worked out from it on the scan thread.
struct Scanned {
    result: ScanResult,
    kinds: Vec<Kind>,
    /// Folder sizes from earlier scans of the same folder, oldest first.
    earlier: Vec<Snapshot>,
    /// For a whole drive: its used space and what the system says about
    /// space outside the files.
    hidden: Option<Hidden>,
    /// Opened from a scan file rather than scanned here.
    imported: bool,
}

struct RunningDelete {
    node: NodeId,
    name: String,
    size: u64,
    result: Slot<Result<(), String>>,
}

/// What the app sheet is showing.
enum SheetTarget {
    App(String),
    Removed(String),
    /// The FreeSize Hub settings.
    Hub,
    /// The folders of old screenshots or chat media.
    Review(ReviewKind),
}

/// Ids of the review sheet's folder buttons.
const REVIEW_IDS: [&str; 8] = [
    "review-0", "review-1", "review-2", "review-3", "review-4", "review-5", "review-6", "review-7",
];

/// Waits for the user to come back from a system screen (uninstall dialog,
/// app info, clear cache) so the app sizes can be measured again.
struct ReturnWatch {
    left: bool,
    ticks: u64,
    /// Seconds until giving up if the screen never opened.
    patience: u64,
}

/// What a basket job does with every item.
enum BasketAction {
    Delete,
    /// Into this folder, keeping the folders below the scanned one.
    Move(PathBuf),
}

/// What a basket job did.
struct BasketOutcome {
    done: Vec<NodeId>,
    failed: Vec<String>,
    bytes: u64,
    /// For a move: the record to put it back.
    moved: Option<moves::Manifest>,
}

/// The duplicate search in the scanned folder.
enum DupeSearch {
    Idle,
    Running {
        progress: Arc<DuplicateProgress>,
        result: Slot<Vec<DuplicateGroup>>,
    },
    Done(Vec<DuplicateGroup>),
}

/// A search for a folder review on a worker thread: its progress (and way
/// to stop it) and where its result lands.
struct ReviewJob<T> {
    progress: Arc<DuplicateProgress>,
    result: Slot<T>,
}

impl<T> ReviewJob<T> {
    fn running(&self) -> (u64, u64) {
        (
            self.progress.done.load(Ordering::Relaxed),
            self.progress.files.load(Ordering::Relaxed),
        )
    }
}

impl<T> Drop for ReviewJob<T> {
    fn drop(&mut self) {
        self.progress.cancel();
    }
}

/// Previews made of a reviewed folder's photos, not yet shown.
type Previews = Vec<(NodeId, Option<owners::Pixels>)>;

/// The search for similar photos in the scanned folder.
enum PhotoSearch {
    Idle,
    Running {
        progress: Arc<similar::Progress>,
        result: Slot<Vec<similar::Group>>,
    },
    Done {
        groups: Vec<similar::Group>,
        previews: HashMap<NodeId, Image>,
    },
}

struct ScanSummary {
    seconds: f64,
    errors: u64,
    cancelled: bool,
}

pub struct Controller {
    me: Weak<RefCell<Controller>>,
    ui: slint::Weak<AppWindow>,
    tree: Option<Tree>,
    rows: Rc<TreeRows>,
    home_rows: Rc<VecModel<HomeRow>>,
    cleanup_rows: Rc<VecModel<CleanRow>>,
    /// The Clean up tab's rows above the duplicates.
    cleanup_found: Vec<filecleanup::Section>,
    /// The Clean up tab's sections as last shown, for its buttons and the
    /// report.
    cleanup_shown: Vec<filecleanup::Section>,
    cleanup_sections: Rc<VecModel<CleanSection>>,
    /// The Clean up page open; empty for the overview.
    cleanup_page: String,
    /// The type of duplicates listed; `None` for every type.
    dupe_filter: Option<String>,
    mode: SizeMode,
    selected: NodeId,
    /// The folder shown in the treemap and the file lists.
    focus: NodeId,
    map_size: (f32, f32),
    bubble_size: (f32, f32),
    /// What each node mostly holds, for icons.
    kinds: Vec<Kind>,
    /// Folders a tool can rebuild, outlined in the map views.
    rebuildable: HashSet<NodeId>,
    /// Which app or game a folder belongs to, for its icon.
    owners: Owners,
    /// The place shown: Home, Clean, Settings, …
    section: Section,
    /// The app group shown as bubbles, zoomed into an app if set.
    app_bubbles: bool,
    app_zoom: Option<String>,
    app_bubble_size: (f32, f32),
    app_bubble_rows: Rc<VecModel<BubbleItem>>,
    /// What the files list is limited to.
    file_filter: Option<FileFilter>,
    /// The age and size groups last shown, for tapping them.
    bins: RefCell<(Vec<Bin>, Vec<Bin>)>,
    /// Folder sizes of earlier scans of the scanned folder, oldest first,
    /// and the one the Changes tab compares with.
    earlier: Vec<Snapshot>,
    compare: Option<usize>,
    /// The Changes chart's plot, in pixels.
    chart_size: (f32, f32),
    /// Space no file accounts for, when a whole drive was scanned.
    hidden: Option<Hidden>,
    /// Items collected to delete or move at once, and what's being done
    /// with them.
    basket: Vec<NodeId>,
    pending_basket: bool,
    basket_job: Option<Slot<BasketOutcome>>,
    /// The last move, until it's put back.
    last_move: Option<moves::Manifest>,
    put_back_job: Option<Slot<(usize, Vec<String>)>>,
    /// Folders offered as move destinations on phones.
    move_targets: Vec<PathBuf>,
    /// The tree came from a scan file: its files may not be on this device,
    /// so nothing can be opened, deleted or moved.
    imported: bool,
    /// The window Windows opened as administrator for one scan.
    elevated: bool,
    /// A root scan running in the background, to open when it's done.
    exporting: Option<elevate::Exporting>,
    change_rows: Rc<VecModel<CleanRow>>,
    summary: Option<ScanSummary>,
    scan: Option<RunningScan>,
    deletion: Option<RunningDelete>,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    awaiting_permission: Option<String>,
    apps: AppsState,
    apps_job: Option<Slot<Result<AppReport, String>>>,
    /// Waiting for the user to come back from the Usage access screen.
    awaiting_usage_access: bool,
    /// The group of apps opened as its own page over the home screen.
    page: Option<View>,
    /// Where the home screen was scrolled to when the page opened.
    home_scroll: f32,
    sheet: Option<SheetTarget>,
    /// The connection to a FreeSize Hub, if paired.
    hub: Hub,
    hub_status: String,
    /// The update check and installer, with the Updates section's state.
    updater: Updater,
    /// What the quarantine held when last looked, for the Clean up tab and
    /// the home screen.
    quarantine: Vec<freesize_proto::unraid::QuarantineBatch>,
    /// A Put back or Empty running on a worker thread.
    quarantine_job: Option<Slot<Result<String, String>>>,
    /// Batches to empty once the confirmation is answered.
    pending_empty: Option<Vec<String>>,
    /// Picks up the Hub's events; slower than `timer`.
    hub_timer: Timer,
    /// Requests from the Hub waiting to be shown, and the one on screen.
    prompts: VecDeque<hub::Prompt>,
    prompt: Option<hub::Prompt>,
    /// Pairing: the link being tried, and the Hub to confirm by fingerprint.
    hub_link: Option<freesize_proto::PairingLink>,
    hub_hello: Option<freesize_proto::Hello>,
    /// Junk last found, for cleaning it when the Hub asks.
    hub_junk: Arc<Mutex<Vec<cleanup::Junk>>>,
    disks_reported: Option<std::time::Instant>,
    return_watch: Option<ReturnWatch>,
    apk_job: Option<Slot<Result<String, String>>>,
    /// Junk waiting for the user to confirm deleting it.
    pending_cleanup: Option<JunkKind>,
    /// Files to show once the scan being run finishes, limited to a filter
    /// if one is set.
    pending_files: Option<Option<FileFilter>>,
    /// Bytes freed and failures of a running cleanup.
    cleanup_job: Option<Slot<(u64, Vec<String>)>>,
    /// Total cache when Android's clear-cache dialog was opened, and
    /// whether the user is back from it.
    cache_check: Option<(u64, bool)>,
    /// What that dialog freed the last time.
    cache_freed: Option<u64>,
    /// Measure the apps again when the running measurement ends, because it
    /// started before something changed.
    remeasure: bool,
    dupes: DupeSearch,
    photos: PhotoSearch,
    /// The folder under review on the Clean up tab, and its searches:
    /// words inside files, copies elsewhere, previews of its photos (handed
    /// over as they're made).
    review: Option<review::Review>,
    review_settings: review::Settings,
    review_words: Option<ReviewJob<HashMap<NodeId, Vec<String>>>>,
    review_copies: Option<ReviewJob<(bool, HashMap<NodeId, NodeId>)>>,
    review_previews: Option<ReviewJob<Previews>>,
    /// Past sizes of the drives on the start screen.
    drive_log: DriveLog,
    /// A file to open once Android has indexed it, and how many times that
    /// was tried.
    pending_open: Option<(PathBuf, u32)>,
    /// The Clean junk screen (desktops): its findings, rows and running
    /// look or clean.
    junk: junk::State,
    junk_rows: Rc<VecModel<JunkRow>>,
    junk_find_job: Option<Slot<Vec<freesize_core::junk::Finding>>>,
    junk_clean_job: Option<Slot<junk::Outcome>>,
    /// The running look was the reminder's: it updates the home screen's
    /// card without opening the screen.
    junk_quiet: bool,
    /// The chosen junk waits for the confirmation.
    pending_junk: bool,
    /// The tool rule whose command runs once the confirmation is accepted.
    pending_command: Option<usize>,
    /// The phone's Clean junk screen waits for the apps report.
    junk_after_apps: bool,
    /// A quarantined batch's page, while it's open.
    quarantine_view: Option<quarantine::View>,
    /// Items of a batch to delete for good once confirmed.
    pending_empty_items: Option<(String, Vec<String>)>,
    /// Items of a batch to put back once a conflict is answered.
    pending_restore: Option<(String, Vec<String>)>,
    /// A tool rule's command running, and what it printed.
    command_job: Option<(&'static str, Slot<Result<String, String>>)>,
    timer: Timer,
    ticks: u64,
}

/// Registers a UI callback that runs `$body` with the controller borrowed.
macro_rules! on {
    ($ui:expr, $setter:ident, $weak:expr, |$c:ident $(, $arg:ident)*| $body:expr) => {{
        let weak = $weak.clone();
        $ui.$setter(move |$($arg),*| {
            Controller::run(&weak, move |$c: &mut Controller| {
                $body;
            });
        });
    }};
}

/// The text of the button beside Scan, for a scan with more rights; empty
/// where there's none (phones, or this window already has them).
fn admin_label(elevated: bool) -> String {
    if elevated || cfg!(target_os = "android") {
        return String::new();
    }
    let offer = elevate::offer(
        freesize_core::access::rights(),
        &locations::default_path(),
        false,
    );
    if offer.action == elevate::SCAN_AS_ADMIN && cfg!(windows) {
        "Run as administrator".into()
    } else {
        offer.action
    }
}

/// The phone's model or the computer's name, for the Hub.
fn device_name() -> String {
    #[cfg(target_os = "android")]
    return crate::android::device_name();
    #[cfg(not(target_os = "android"))]
    freesize_agent::hostname()
}

fn os_name() -> String {
    #[cfg(target_os = "android")]
    return crate::android::os_name();
    #[cfg(not(target_os = "android"))]
    freesize_proto::Platform::current().label().to_string()
}

type SharedTransport = Arc<dyn freesize_agent::Transport + Send + Sync>;

/// This platform's HTTP client, for the Hub and for update checks.
fn transport() -> SharedTransport {
    #[cfg(target_os = "android")]
    return Arc::new(crate::android::JavaTransport);
    #[cfg(not(target_os = "android"))]
    Arc::new(freesize_agent::UreqTransport::new())
}

/// The Hub connection, with this platform's HTTP client and actions.
fn new_hub(junk: &Arc<Mutex<Vec<cleanup::Junk>>>, transport: SharedTransport) -> Hub {
    let path = growth::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hub.json");
    let executor = Arc::new(hub::AppExecutor { junk: junk.clone() });
    let policy = hub::policy(&path);
    Hub::new(path, transport, executor, policy)
}

/// Creates the controller and connects it to `ui`. Keep the returned value
/// alive for as long as the window runs.
/// The status line while nothing is scanned.
const IDLE_HINT: &str = "Pick a drive or folder, or type a path and press Scan.";

pub fn install(
    ui: &AppWindow,
    initial: Option<PathBuf>,
    elevated: bool,
) -> Rc<RefCell<Controller>> {
    let hub_junk: Arc<Mutex<Vec<cleanup::Junk>>> = Arc::default();
    let transport = transport();
    let ctl = Rc::new_cyclic(|me| {
        RefCell::new(Controller {
            me: me.clone(),
            ui: ui.as_weak(),
            tree: None,
            rows: Rc::new(TreeRows::default()),
            home_rows: Rc::new(VecModel::default()),
            cleanup_rows: Rc::new(VecModel::default()),
            cleanup_found: Vec::new(),
            cleanup_shown: Vec::new(),
            cleanup_sections: Rc::new(VecModel::default()),
            cleanup_page: String::new(),
            dupe_filter: None,
            review: None,
            review_settings: review::Settings::load(),
            review_words: None,
            review_copies: None,
            review_previews: None,
            mode: SizeMode::Allocated,
            selected: 0,
            focus: 0,
            map_size: (0.0, 0.0),
            bubble_size: (0.0, 0.0),
            kinds: Vec::new(),
            rebuildable: HashSet::new(),
            owners: Owners::default(),
            section: Section::Home,
            app_bubbles: false,
            app_zoom: None,
            app_bubble_size: (0.0, 0.0),
            app_bubble_rows: Rc::new(VecModel::default()),
            file_filter: None,
            bins: RefCell::default(),
            earlier: Vec::new(),
            compare: None,
            chart_size: (600.0, 120.0),
            hidden: None,
            basket: Vec::new(),
            pending_basket: false,
            basket_job: None,
            last_move: moves::last(),
            put_back_job: None,
            move_targets: Vec::new(),
            imported: false,
            elevated,
            exporting: None,
            change_rows: Rc::new(VecModel::default()),
            summary: None,
            scan: None,
            deletion: None,
            awaiting_permission: None,
            apps: AppsState::Unsupported,
            apps_job: None,
            awaiting_usage_access: false,
            page: None,
            home_scroll: 0.0,
            sheet: None,
            hub: new_hub(&hub_junk, transport.clone()),
            hub_status: String::new(),
            updater: Updater::new(growth::data_dir(), transport.clone()),
            quarantine: quarantine::list(),
            quarantine_job: None,
            pending_empty: None,
            hub_timer: Timer::default(),
            prompts: VecDeque::new(),
            prompt: None,
            hub_link: None,
            hub_hello: None,
            hub_junk: hub_junk.clone(),
            disks_reported: None,
            return_watch: None,
            apk_job: None,
            pending_cleanup: None,
            pending_files: None,
            cleanup_job: None,
            cache_check: None,
            cache_freed: None,
            remeasure: false,
            dupes: DupeSearch::Idle,
            photos: PhotoSearch::Idle,
            drive_log: DriveLog::default(),
            pending_open: None,
            junk: {
                let mut state = junk::State::new(growth::data_dir());
                state.elevated = elevated || freesize_core::access::rights().sees_everything();
                state
            },
            junk_rows: Rc::new(VecModel::default()),
            junk_find_job: None,
            junk_clean_job: None,
            junk_quiet: false,
            pending_junk: false,
            pending_command: None,
            junk_after_apps: false,
            quarantine_view: None,
            pending_empty_items: None,
            pending_restore: None,
            command_job: None,
            timer: Timer::default(),
            ticks: 0,
        })
    });

    ui.set_tree_rows(ModelRc::from(ctl.borrow().rows.clone()));
    ui.set_home_rows(ModelRc::from(ctl.borrow().home_rows.clone()));
    ui.set_cleanup_rows(ModelRc::from(ctl.borrow().cleanup_rows.clone()));
    ui.set_cleanup_sections(ModelRc::from(ctl.borrow().cleanup_sections.clone()));
    ui.set_app_bubbles(ModelRc::from(ctl.borrow().app_bubble_rows.clone()));
    ui.set_change_rows(ModelRc::from(ctl.borrow().change_rows.clone()));
    ui.set_junk_rows(ModelRc::from(ctl.borrow().junk_rows.clone()));
    let start = initial
        .clone()
        .filter(|p| p.as_os_str() != elevate::JUNK_FLAG)
        .unwrap_or_else(locations::default_path);
    ui.set_path(start.to_string_lossy().as_ref().into());
    ui.set_can_browse(actions::CAN_BROWSE);
    ui.set_can_reveal(actions::CAN_REVEAL);
    ui.set_delete_label(actions::DELETE_LABEL.into());
    ui.set_status(IDLE_HINT.into());
    ui.set_admin_label(admin_label(elevated).into());
    if elevated {
        ui.set_window_title("FreeSize (administrator)".into());
        ui.set_status(
            "Running as administrator for this scan: other users' folders are read too. \
             Deleting follows the same rules as always."
                .into(),
        );
    }

    let weak = Rc::downgrade(&ctl);
    on!(ui, on_start_scan, weak, |c, path| c
        .start_scan(path.to_string()));
    on!(ui, on_cancel_scan, weak, |c| c.cancel_scan());
    on!(ui, on_show_locations, weak, |c| c.show_locations());
    on!(ui, on_toggle_row, weak, |c, row| c.toggle_row(row));
    on!(ui, on_select_node, weak, |c, id, reveal| c
        .select(id, reveal));
    on!(ui, on_map_resized, weak, |c, w, h| c.map_resized(w, h));
    on!(ui, on_change_chart_resized, weak, |c, w, h| {
        c.chart_size = (w, h);
        c.refresh_changes();
    });
    on!(ui, on_bubbles_resized, weak, |c, w, h| c
        .bubbles_resized(w, h));
    on!(ui, on_map_view_changed, weak, |c| c.update_map());
    on!(ui, on_app_bubble, weak, |c, key| c.app_bubble(key.as_str()));
    on!(ui, on_type_picked, weak, |c, key| c
        .pick_group(key.as_str()));
    on!(ui, on_clear_files_filter, weak, |c| c.clear_file_filter());
    on!(ui, on_collect_listed, weak, |c| c.collect_listed());
    on!(ui, on_types_view_changed, weak, |c| c.update_lists());
    on!(ui, on_compare_scan, weak, |c, index| c.compare_with(index));
    on!(ui, on_collect, weak, |c| c.toggle_collect());
    on!(ui, on_basket_review, weak, |c| c.open_basket(true));
    on!(ui, on_basket_close, weak, |c| c.open_basket(false));
    on!(ui, on_basket_remove, weak, |c, id| c.remove_from_basket(id));
    on!(ui, on_basket_clear, weak, |c| c.clear_basket());
    on!(ui, on_basket_delete_all, weak, |c| c
        .request_basket_delete());
    on!(ui, on_move_target, weak, |c, i| c.move_to_target(i));
    on!(ui, on_put_back, weak, |c| c.put_back());
    on!(ui, on_app_bubbles_resized, weak, |c, w, h| c
        .app_bubbles_resized(w, h));
    on!(ui, on_size_mode_changed, weak, |c| c.size_mode_changed());
    on!(ui, on_go_up, weak, |c| c.go_up());
    on!(ui, on_reveal_selected, weak, |c| c.reveal_selected());
    on!(ui, on_open_selected, weak, |c| c.open_selected());
    on!(ui, on_delete_requested, weak, |c| c.request_delete());
    on!(ui, on_delete_confirmed, weak, |c| c.confirm_delete());
    on!(ui, on_delete_cancelled, weak, |c| c.cancel_delete());
    on!(ui, on_grant_permission, weak, |c| c.grant_permission());
    on!(ui, on_open_app, weak, |c, package| c
        .open_app(package.as_str()));
    on!(ui, on_home_action, weak, |c, id| c.home_action(id.as_str()));
    on!(ui, on_home_text, weak, |c, key, text| c
        .home_text(key.as_str(), text.as_str()));
    on!(ui, on_home_choice, weak, |c, key, index| c
        .home_choice(key.as_str(), index));
    on!(ui, on_section_picked, weak, |c, key| c
        .show_section(Section::from_key(key.as_str())));
    on!(ui, on_scan_as_admin, weak, |c| c.scan_as_admin());
    on!(ui, on_hub_connect, weak, |c| c.hub_connect());
    on!(ui, on_hub_confirm, weak, |c| c.hub_confirm());
    on!(ui, on_hub_connect_cancel, weak, |c| c.hub_connect_cancel());
    on!(ui, on_approval_answer, weak, |c, answer| c
        .approval_answer(answer));
    on!(ui, on_approval_later, weak, |c| c.approval_later());
    // Export buttons save through a modal dialog, so they run without
    // holding the controller; other sheet buttons go to the controller.
    let sheet_weak = weak.clone();
    ui.on_sheet_action(move |id| {
        let id = id.to_string();
        if [EXPORT_HTML, EXPORT_CSV, EXPORT_JSON].contains(&id.as_str()) {
            let Some(rc) = sheet_weak.upgrade() else {
                return;
            };
            let prepared = rc
                .try_borrow_mut()
                .ok()
                .and_then(|mut c| c.prepare_export(&id));
            if let Some((name, bytes)) = prepared {
                let saved = actions::save_export(&name, &bytes);
                Controller::run(&sheet_weak, move |c| c.finish_export(saved));
            }
            return;
        }
        Controller::run(&sheet_weak, move |c| c.sheet_action(&id));
    });
    on!(ui, on_export, weak, |c| c.open_export());
    on!(ui, on_cleanup_action, weak, |c, key| c.cleanup_action(&key));
    on!(ui, on_cleanup_open, weak, |c, key| c.cleanup_open(&key));
    on!(ui, on_cleanup_open_item, weak, |c, id| c
        .cleanup_open_item(id));
    on!(ui, on_cleanup_check, weak, |c, id| c.cleanup_check(id));
    on!(ui, on_cleanup_group, weak, |c, index| c
        .cleanup_group(index));
    on!(ui, on_cleanup_pick, weak, |c, all| c.cleanup_pick(all));
    on!(ui, on_review_folder, weak, |c, id| c.review_folder(id));
    on!(ui, on_cleanup_filter, weak, |c, index| c
        .cleanup_filter(index));
    on!(ui, on_junk_close, weak, |c| c.junk_open(false));
    on!(ui, on_junk_toggle, weak, |c, index| c.junk_toggle(index));
    on!(ui, on_junk_expand, weak, |c, index| c.junk_expand(index));
    on!(ui, on_junk_show, weak, |c, id| c.junk_show(id));
    on!(ui, on_junk_tool, weak, |c, index| c.junk_tool(index));
    on!(ui, on_junk_clean, weak, |c| c.request_junk_clean());
    on!(ui, on_junk_again, weak, |c| c.junk_find(false));
    on!(ui, on_junk_stop, weak, |c| c.junk_stop());
    on!(ui, on_qv_back, weak, |c| c.qv_back());
    on!(ui, on_qv_toggle, weak, |c, id| c.qv_toggle(id));
    on!(ui, on_qv_open_item, weak, |c, id| c.qv_open_item(id));
    on!(ui, on_qv_reveal, weak, |c, id| c.qv_reveal(id));
    on!(ui, on_qv_restore, weak, |c, id| c.qv_restore(Some(id)));
    on!(ui, on_qv_delete, weak, |c, id| c.qv_delete(Some(id)));
    on!(ui, on_qv_restore_chosen, weak, |c| c.qv_restore(None));
    on!(ui, on_qv_delete_chosen, weak, |c| c.qv_delete(None));
    on!(ui, on_conflict_chosen, weak, |c, choice| c
        .conflict_chosen(choice));
    on!(ui, on_junk_done, weak, |c| c.junk_done());
    on!(ui, on_junk_command, weak, |c, index| c.junk_command(index));
    on!(ui, on_junk_run, weak, |c, index| c.request_junk_run(index));
    on!(ui, on_junk_quarantine, weak, |c| {
        c.junk_open(false);
        c.show_section(Section::Quarantine);
    });

    // Folder pickers are modal, so they run without holding the controller.
    let move_weak = weak.clone();
    ui.on_basket_move_all(move || {
        if actions::CAN_BROWSE {
            if let Some(destination) = actions::pick_destination() {
                Controller::run(&move_weak, move |c| {
                    c.run_basket(BasketAction::Move(destination))
                });
            }
        } else {
            Controller::run(&move_weak, |c| c.show_move_targets());
        }
    });
    let ui_weak = ui.as_weak();
    let browse_weak = weak.clone();
    ui.on_browse(move || {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        let current = PathBuf::from(ui.get_path().as_str());
        if let Some(folder) = actions::browse(&current) {
            let folder = folder.to_string_lossy().into_owned();
            ui.set_path(folder.as_str().into());
            Controller::run(&browse_weak, move |c| c.start_scan(folder));
        }
    });

    #[cfg(target_os = "android")]
    if !crate::android::has_storage_access() {
        ui.set_needs_permission(true);
        let mut c = ctl.borrow_mut();
        c.awaiting_permission = Some(String::new());
        c.ensure_ticking();
    }

    {
        let mut c = ctl.borrow_mut();
        c.start_hub_timer();
        // Opened from a scanned pairing QR code: through the PairLink
        // activity since 0.15.1, or straight from the intent by an older
        // manifest.
        #[cfg(target_os = "android")]
        if let Some(link) = c.scanned_link().or_else(|| {
            crate::android::launch_link().filter(|l| freesize_proto::pairing::is_link(l))
        }) {
            c.open_hub_connect(&link);
        }
        c.record_drives();
        c.junk_remind();
        if apps::supported() {
            if apps::has_access() {
                c.start_apps_job();
            } else {
                c.apps = AppsState::NeedsAccess { asked: false };
            }
        }
        c.refresh_home();
    }

    match initial {
        // The window as administrator, opened to run a Clean junk command.
        Some(path) if path.as_os_str() == elevate::JUNK_FLAG => {
            let mut c = ctl.borrow_mut();
            c.junk_open(true);
            c.junk_find(false);
        }
        Some(path) => ctl
            .borrow_mut()
            .start_scan(path.to_string_lossy().into_owned()),
        None => {}
    }
    ctl
}

impl Controller {
    /// Runs `job` with the controller borrowed. If it is already borrowed (a
    /// callback fired while Rust was updating the UI), retries right after.
    fn run(weak: &Weak<RefCell<Controller>>, job: impl FnOnce(&mut Controller) + 'static) {
        let Some(rc) = weak.upgrade() else {
            return;
        };
        if let Ok(mut c) = rc.try_borrow_mut() {
            job(&mut c);
            return;
        }
        let weak = weak.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(rc) = weak.upgrade()
                && let Ok(mut c) = rc.try_borrow_mut()
            {
                job(&mut c);
            }
        });
    }

    fn ui(&self) -> Option<AppWindow> {
        self.ui.upgrade()
    }

    fn ensure_ticking(&self) {
        if self.timer.running() {
            return;
        }
        let weak = self.me.clone();
        self.timer
            .start(TimerMode::Repeated, Duration::from_millis(100), move || {
                if let Some(rc) = weak.upgrade()
                    && let Ok(mut c) = rc.try_borrow_mut()
                {
                    c.tick();
                }
            });
    }

    /// Polls background work: scan progress and results, deletions, and on
    /// Android whether storage access was granted.
    fn tick(&mut self) {
        self.ticks += 1;

        if let Some(running) = &self.scan {
            match take(&running.result) {
                Some(result) => {
                    let running = self.scan.take().expect("scan is running");
                    self.finish_scan(running.path, result);
                }
                None => self.show_progress(),
            }
        }

        if let Some(running) = &self.deletion
            && let Some(result) = take(&running.result)
        {
            let running = self.deletion.take().expect("deletion is running");
            self.finish_delete(running, result);
        }

        if let Some(exporting) = &mut self.exporting
            && let Some(result) = exporting.poll()
        {
            let exporting = self.exporting.take().expect("a root scan is running");
            match result {
                // The result opens as a saved scan: read-only, as a scan made
                // with other rights must be.
                Ok(file) => self.start_scan(file.to_string_lossy().into_owned()),
                Err(reason) => self.set_status(&format!(
                    "The scan of {} with more rights didn't happen: {reason}.",
                    exporting.root.display()
                )),
            }
        }

        #[cfg(target_os = "android")]
        if self.awaiting_permission.is_some()
            && self.ticks.is_multiple_of(10)
            && crate::android::has_storage_access()
        {
            let pending = self.awaiting_permission.take().unwrap_or_default();
            if let Some(ui) = self.ui() {
                ui.set_needs_permission(false);
            }
            // The clear-cache card changes once access is granted.
            self.refresh_home();
            if !pending.is_empty() {
                self.start_scan(pending);
            }
        }

        if let Some(job) = &self.apps_job
            && let Some(result) = take(job)
        {
            self.apps_job = None;
            self.apps = match result {
                Ok(report) => AppsState::Ready(report),
                Err(err) => AppsState::Failed(err),
            };
            if std::mem::take(&mut self.junk_after_apps) {
                self.junk_from_report();
            }
            if let AppsState::Ready(report) = &self.apps {
                // Folders of apps in Android/media and the like get their icons.
                self.owners.set_android_icons(report.icons.clone());
                *self.hub_junk.lock().unwrap_or_else(|e| e.into_inner()) = report.junk.clone();
                self.hub.report(freesize_proto::Report {
                    apps: Some(hub::apps_report(report)),
                    ..freesize_proto::Report::default()
                });
                self.update_map();
            }
            if self.remeasure {
                self.remeasure = false;
                self.start_apps_job();
            } else if let (Some((before, true)), Some(report)) = (self.cache_check, self.report()) {
                let after = home::clearable_cache(report, apps::abilities());
                let freed = before.saturating_sub(after);
                self.cache_check = None;
                self.cache_freed = Some(freed);
                self.set_status(&if freed == 0 {
                    "Android's cache cleaner freed nothing: the caches in shared storage were \
                     already empty, or it was cancelled. The rest only clears app by app."
                        .to_string()
                } else {
                    format!("Android's cache cleaner freed {}.", format_size(freed))
                });
            }
            self.refresh_home();
        }

        if self.awaiting_usage_access && self.ticks.is_multiple_of(10) && apps::has_access() {
            self.awaiting_usage_access = false;
            self.start_apps_job();
        }

        if let Some(watch) = &mut self.return_watch {
            watch.ticks += 1;
            if watch.ticks.is_multiple_of(5) {
                let focused = apps::window_has_focus();
                let back = watch.left && focused;
                watch.left |= !focused;
                // Give up if the screen never opened, or after 10 minutes.
                let never_opened = !watch.left && watch.ticks > watch.patience * 10;
                let expired = never_opened || watch.ticks > 6000;
                if back || expired {
                    self.return_watch = None;
                }
                // Android's cache cleaner closes without a word when it
                // won't run for FreeSize.
                if never_opened && self.cache_check.take().is_some() {
                    self.set_status(
                        "Android didn't open its cache cleaner, so nothing was cleared. Check \
                         that FreeSize has All files access, or clear the apps under Biggest \
                         caches one by one.",
                    );
                }
                if back {
                    if let Some((_, back_from_cache)) = &mut self.cache_check {
                        *back_from_cache = true;
                    }
                    // A measurement already running started before the
                    // change; measure again after it.
                    if self.apps_job.is_some() {
                        self.remeasure = true;
                    }
                    self.start_apps_job();
                }
            }
        }

        if let Some(job) = &self.cleanup_job
            && let Some((freed, errors)) = take(job)
        {
            self.cleanup_job = None;
            let mut text = format!(
                "Moved {} to the quarantine; empty it to free the space.",
                format_size(freed)
            );
            if !errors.is_empty() {
                text.push_str(&format!(
                    " {} couldn't be moved, for example {}",
                    plural(errors.len() as u64, "item", "items"),
                    errors[0]
                ));
            }
            self.set_status(&text);
            self.quarantine_changed();
            self.start_apps_job();
        }

        if let Some(job) = &self.quarantine_job
            && let Some(result) = take(job)
        {
            self.quarantine_job = None;
            self.set_status(&result.unwrap_or_else(|err| format!("Couldn't do that: {err}")));
            self.quarantine_changed();
        }

        if self.updater.busy() || self.updater.due(apps::now() as i64) {
            if self.updater.due(apps::now() as i64) {
                self.updater.check(apps::now() as i64);
            }
            let (status, relaunch) = self.updater.poll();
            if let Some(status) = &status {
                self.set_status(status);
            }
            if status.is_some() || !self.updater.busy() {
                self.refresh_home();
            }
            if let Some(exe) = relaunch
                && let Err(err) = updater::relaunch(&exe)
            {
                self.set_status(&format!(
                    "Updated, but the new FreeSize couldn't be started ({err}); start it yourself."
                ));
            }
        }

        if let DupeSearch::Running { result, .. } = &self.dupes {
            if let Some(groups) = take(result) {
                self.dupes = DupeSearch::Done(groups);
                self.show_cleanup();
            } else if self.ticks.is_multiple_of(5) {
                self.show_cleanup();
            }
        }

        self.tick_review();

        if let PhotoSearch::Running { result, .. } = &self.photos {
            if let Some(groups) = take(result) {
                let previews = groups
                    .iter()
                    .flat_map(|g| g.photos.iter().zip(&g.previews))
                    .filter_map(|(&id, preview)| Some((id, preview.as_ref()?.image())))
                    .collect();
                self.photos = PhotoSearch::Done { groups, previews };
                self.show_cleanup();
            } else if self.ticks.is_multiple_of(5) {
                self.show_cleanup();
            }
        }

        if let Some((path, tries)) = &mut self.pending_open
            && self.ticks.is_multiple_of(5)
        {
            *tries += 1;
            let (path, tries) = (path.clone(), *tries);
            match actions::open(&path) {
                Ok(actions::Open::TryAgain) if tries < 10 => {}
                Ok(actions::Open::TryAgain) => {
                    self.pending_open = None;
                    self.set_status(
                        "Android hasn't indexed this file yet. Try Open again in a moment.",
                    );
                }
                Ok(actions::Open::Done) => {
                    self.pending_open = None;
                    self.refresh_status();
                }
                Err(err) => {
                    self.pending_open = None;
                    self.set_status(&err);
                }
            }
        }

        if let Some(job) = &self.basket_job
            && let Some(outcome) = take(job)
        {
            self.basket_job = None;
            self.finish_basket(outcome);
        }

        if let Some(job) = &self.put_back_job
            && let Some((back, errors)) = take(job)
        {
            self.put_back_job = None;
            self.last_move = moves::last();
            let mut text = format!(
                "Put back {}. Scan again to see {}.",
                plural(back as u64, "item", "items"),
                if back == 1 { "it" } else { "them" }
            );
            if let Some(err) = errors.first() {
                text.push_str(&format!(" {} couldn't: {err}", errors.len()));
            }
            self.set_status(&text);
            self.refresh_basket();
        }

        if let Some(job) = &self.apk_job
            && let Some(result) = take(job)
        {
            self.apk_job = None;
            if let Some(ui) = self.ui() {
                ui.set_status(result.unwrap_or_else(|err| err).into());
            }
        }

        if let Some(job) = &self.junk_find_job
            && let Some(findings) = take(job)
        {
            self.junk_find_job = None;
            self.junk_found(findings);
        }
        if let Some(job) = &self.junk_clean_job
            && let Some(outcome) = take(job)
        {
            self.junk_clean_job = None;
            self.junk.progress = None;
            self.junk.stage = junk::Stage::Done;
            self.junk.outcome = Some(outcome.clone());
            self.set_status(&outcome.summary());
            self.quarantine_changed();
            // What's left, now that some of it went.
            self.junk_find(false);
        } else if self.junk_clean_job.is_some() {
            // The progress page follows the clean.
            self.junk_refresh();
        }
        if let Some((index, job)) = &self.command_job
            && let Some(result) = take(job)
        {
            let id = *index;
            self.command_job = None;
            self.junk.command_running = None;
            let text = match result {
                Ok(text) => text,
                Err(err) => format!("Didn't work: {err}"),
            };
            self.junk.command_output.insert(id, text);
            // Look again: the rule should be smaller now.
            self.junk_find(false);
        }

        let busy = self.scan.is_some()
            || self.deletion.is_some()
            || self.awaiting_permission.is_some()
            || self.apps_job.is_some()
            || self.awaiting_usage_access
            || self.return_watch.is_some()
            || self.apk_job.is_some()
            || self.cleanup_job.is_some()
            || self.quarantine_job.is_some()
            || self.updater.busy()
            || matches!(self.dupes, DupeSearch::Running { .. })
            || matches!(self.photos, PhotoSearch::Running { .. })
            || self.review_words.is_some()
            || self.review_copies.is_some()
            || self.review_previews.is_some()
            || self.pending_open.is_some()
            || self.basket_job.is_some()
            || self.put_back_job.is_some()
            || self.junk_find_job.is_some()
            || self.junk_clean_job.is_some()
            || self.command_job.is_some();
        if !busy {
            self.timer.stop();
        }
    }

    // ----- Scanning -----------------------------------------------------

    fn start_scan(&mut self, path: String) {
        let path = path.trim().to_string();
        let Some(ui) = self.ui() else {
            return;
        };
        if path.is_empty() || self.scan.is_some() {
            return;
        }

        #[cfg(target_os = "android")]
        if !crate::android::has_storage_access() {
            self.awaiting_permission = Some(path);
            ui.set_needs_permission(true);
            crate::android::request_storage_access();
            self.ensure_ticking();
            return;
        }

        let root = PathBuf::from(&path);
        let progress = Arc::new(ScanProgress::new());
        let result: Slot<io::Result<Scanned>> = Arc::default();
        // Deletes the Hub asks for may only reach scanned folders.
        self.hub.add_root(&root);
        let reporter = self.hub.reporter();
        let label = locations::list()
            .into_iter()
            .find(|l| l.path == root)
            .map(|l| l.label)
            .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| path.clone());
        let spawned = {
            let (root, progress, result) = (root.clone(), progress.clone(), result.clone());
            std::thread::Builder::new()
                .name("freesize-scan".into())
                .spawn(move || {
                    // A saved scan file opens instead of being scanned.
                    if is_scan_file(&root) {
                        let started = std::time::Instant::now();
                        let opened = std::fs::File::open(&root)
                            .and_then(|f| ncdu::import(std::io::BufReader::new(f)))
                            .map(|tree| {
                                let kinds = kinds(&tree);
                                Scanned {
                                    result: ScanResult {
                                        tree,
                                        elapsed: started.elapsed(),
                                        errors: 0,
                                        cancelled: false,
                                    },
                                    kinds,
                                    earlier: Vec::new(),
                                    hidden: None,
                                    imported: true,
                                }
                            });
                        put(&result, opened);
                        return;
                    }
                    // Kinds take a pass over the whole tree; do it here
                    // rather than on the UI thread.
                    let scanned = scan(&root, &ScanOptions::default(), &progress).map(|r| {
                        let kinds = kinds(&r.tree);
                        // A cancelled scan is incomplete: nothing to save.
                        let earlier = if r.cancelled {
                            Vec::new()
                        } else {
                            let now = apps::now() as i64;
                            let taken = snapshot::take(&r.tree, now, scans::MAX_FOLDERS);
                            scans::record(&root, &taken)
                        };
                        // A summary for the Hub, made here off the UI thread.
                        if reporter.connected() && !r.cancelled {
                            reporter.send(freesize_proto::Report {
                                scan: Some(freesize_agent::report::scan_report(
                                    &r.tree,
                                    &label,
                                    apps::now() as i64,
                                    r.errors,
                                    true,
                                )),
                                quarantine: Some(quarantine::list()),
                                ..freesize_proto::Report::default()
                            });
                        }
                        let hidden = (!r.cancelled && hidden::is_drive_root(&root))
                            .then(|| disk_space(&root))
                            .flatten()
                            .map(|space| Hidden {
                                used: space.used(),
                                notes: hidden::probe(&root),
                            });
                        Scanned {
                            result: r,
                            kinds,
                            earlier,
                            hidden,
                            imported: false,
                        }
                    });
                    put(&result, scanned)
                })
        };
        if let Err(err) = spawned {
            ui.set_status(format!("Could not start scanning: {err}").into());
            return;
        }

        self.stop_duplicates();
        self.stop_photos();
        self.scan = Some(RunningScan {
            path: root,
            progress,
            result,
        });
        ui.set_path(path.as_str().into());
        ui.set_scan_title(
            if self.elevated {
                format!("Scanning {path} as administrator")
            } else {
                format!("Scanning {path}")
            }
            .into(),
        );
        ui.set_scan_counts("Starting…".into());
        ui.set_scan_current(SharedString::new());
        ui.set_status("Scanning…".into());
        ui.set_scanning(true);
        // A scan started from anywhere (an app's files, old screenshots)
        // shows on Home.
        if self.section != Section::Home {
            self.section = Section::Home;
            self.refresh_home();
        }
        self.ensure_ticking();
    }

    fn cancel_scan(&mut self) {
        if let Some(running) = &self.scan {
            running.progress.cancel();
            if let Some(ui) = self.ui() {
                ui.set_scan_title("Cancelling…".into());
            }
        }
    }

    fn show_progress(&self) {
        let (Some(ui), Some(running)) = (self.ui(), &self.scan) else {
            return;
        };
        let snap = running.progress.snapshot();
        ui.set_scan_counts(
            format!(
                "{} files · {} folders · {}",
                format_count(snap.files),
                format_count(snap.dirs),
                format_size(snap.bytes)
            )
            .into(),
        );
        ui.set_scan_current(snap.current.to_string_lossy().as_ref().into());
    }

    fn finish_scan(&mut self, path: PathBuf, result: io::Result<Scanned>) {
        let Some(ui) = self.ui() else {
            return;
        };
        ui.set_scanning(false);
        let result = match result {
            Ok(scanned) => {
                self.kinds = scanned.kinds;
                self.earlier = scanned.earlier;
                self.hidden = scanned.hidden;
                self.imported = scanned.imported;
                self.compare = changes::default_compare(&self.earlier, apps::now() as i64);
                scanned.result
            }
            Err(err) => {
                self.pending_files = None;
                ui.set_status(format!("Could not scan {}: {err}", path.display()).into());
                return;
            }
        };

        self.summary = Some(ScanSummary {
            seconds: result.elapsed.as_secs_f64(),
            errors: result.errors,
            cancelled: result.cancelled,
        });
        self.owners.clear();
        self.file_filter = None;
        if let Some(filter) = self.pending_files.take() {
            self.file_filter = filter;
            // The "Largest files" tab, or "Largest" on phones.
            ui.set_wide_tab(1);
            ui.set_compact_tab(2);
        }
        self.basket.clear();
        self.cleanup_page.clear();
        self.dupe_filter = None;
        self.close_review();
        let tree = self.tree.insert(result.tree);
        self.rebuildable = rebuildable(tree, tree.root())
            .into_iter()
            .map(|r| r.node)
            .collect();
        self.selected = tree.root();
        self.focus = tree.root();
        self.rows.rebuild(tree, self.mode, &[tree.root()]);
        ui.set_tree_scroll(0.0);
        ui.set_has_tree(true);
        self.section = Section::Home;
        ui.set_section(self.section.key().into());
        self.refresh_focus_views();
        self.refresh_selection();
        self.refresh_status();
        // The selection was of the old tree: show it emptied.
        self.refresh_basket();
        self.refresh_cleanup();
    }

    fn show_locations(&mut self) {
        let Some(ui) = self.ui() else {
            return;
        };
        self.stop_duplicates();
        self.stop_photos();
        self.tree = None;
        self.summary = None;
        self.rows.clear();
        ui.set_map_rects(ModelRc::default());
        ui.set_largest_files(ModelRc::default());
        ui.set_file_types(ModelRc::default());
        self.cleanup_found.clear();
        self.cleanup_shown.clear();
        self.cleanup_rows.clear();
        self.cleanup_sections.clear();
        self.cleanup_page.clear();
        self.dupe_filter = None;
        self.close_review();
        self.change_rows.clear();
        self.basket.clear();
        self.refresh_basket();
        self.earlier.clear();
        self.compare = None;
        ui.set_has_tree(false);
        ui.set_status(IDLE_HINT.into());
        // Free space and app sizes may have changed since the last visit.
        if matches!(self.apps, AppsState::Ready(_) | AppsState::Failed(_)) {
            self.start_apps_job();
        }
        self.record_drives();
        self.refresh_home();
    }

    /// Updates the home screen in place: only rows that differ change, so
    /// the list keeps its scroll position and the measured heights of rows
    /// that stay the same.
    fn refresh_home(&self) {
        // A page lists measured apps; until they are, the group list shows.
        let page = self
            .page
            .filter(|_| self.report().is_some() && self.section == Section::Apps);
        let now = apps::now() as i64;
        if let Some(ui) = self.ui() {
            ui.set_home_page_open(page.is_some());
            ui.set_section(self.section.key().into());
            ui.set_nav_items(ModelRc::new(VecModel::from(self.nav_items())));
        }
        let card = home::HubCard {
            hub: self.hub.state().map(|s| s.hub_name),
            status: self.hub_status.clone(),
            waiting: self.prompts.len(),
        };
        let options = home::Options {
            section: self.section,
            page,
            refreshing: self.apps_job.is_some() && matches!(self.apps, AppsState::Ready(_)),
            storage_access: storage_access(),
            abilities: apps::abilities(),
            cache_freed: self.cache_freed,
            drives: Some(&self.drive_log),
            bubbles: self.app_bubbles,
            zoom: self.app_zoom.as_deref(),
            hub: Some(&card),
        };
        let mut rows = home::rows(&self.apps, options);
        if page.is_none() {
            if self.section == Section::Clean && !phone_junk() {
                rows.extend(self.junk.home_rows(now));
            }
            if self.section.has_quarantine() {
                rows.extend(quarantine::section_rows(
                    &self.quarantine,
                    now,
                    self.section == Section::Quarantine,
                ));
            }
            if self.section == Section::Settings {
                if !cfg!(target_os = "android") {
                    rows.insert(0, home::heading("Clean junk", "", None));
                    rows.insert(1, self.junk.settings_row());
                }
                rows.push(home::heading("Review a folder", "", None));
                rows.push(home::text_setting(
                    "Words to look for inside files",
                    "Look inside files, on a folder's review page, finds text, Office and PDF \
                     files containing these words, separated by commas. It runs on this device \
                     and only says which word a file contains, never the text.",
                    review::WORDS_KEY,
                    &self.review_settings.words.join(", "),
                ));
                rows.extend(self.updater.rows(now));
            }
        }
        sync_model(&self.home_rows, rows);
        self.update_app_bubbles(false);
    }

    /// The sidebar's or tab bar's places, with counts worth seeing.
    fn nav_items(&self) -> Vec<NavItem> {
        let waiting = if self.prompts.is_empty() {
            String::new()
        } else {
            self.prompts.len().to_string()
        };
        let quarantined: usize = self.quarantine.iter().map(|b| b.items.len()).sum();
        Section::all()
            .iter()
            .map(|&section| NavItem {
                key: section.key().into(),
                label: section.label().into(),
                badge: if section.has_hub() {
                    waiting.clone()
                } else if section == Section::Quarantine && quarantined > 0 {
                    quarantined.to_string()
                } else {
                    String::new()
                }
                .into(),
            })
            .collect()
    }

    /// Shows one of the app's places, from its top.
    fn show_section(&mut self, section: Section) {
        if section == self.section {
            // Tapping the current place again closes a group's page.
            if self.page.is_some() {
                self.page = None;
                self.app_bubbles = false;
                self.app_zoom = None;
            } else if let Some(ui) = self.ui() {
                ui.set_home_scroll(0.0);
            }
            self.refresh_home();
            return;
        }
        self.section = section;
        if let Some(ui) = self.ui() {
            ui.set_home_scroll(0.0);
            // The hint about scanning belongs on Home only.
            if section != Section::Home && ui.get_status() == IDLE_HINT {
                ui.set_status("".into());
            } else if section == Section::Home && ui.get_status().is_empty() {
                ui.set_status(IDLE_HINT.into());
            }
        }
        self.refresh_home();
    }

    /// A setting's drop-down in Settings.
    fn home_choice(&mut self, key: &str, index: i32) {
        let index = usize::try_from(index).unwrap_or(0);
        match key {
            junk::REMIND => {
                let Some(remind) = junk::REMIND_CHOICES.get(index) else {
                    return;
                };
                if self.junk.settings.remind != *remind {
                    self.junk.settings.remind = (*remind).into();
                    self.junk.save();
                    self.junk_remind();
                }
            }
            updater::MODE => {
                self.updater.set_mode(index, apps::now() as i64);
                self.ensure_ticking();
            }
            _ => return,
        }
        self.refresh_home();
    }

    /// A setting typed in Settings.
    fn home_text(&mut self, key: &str, text: &str) {
        if key == review::WORDS_KEY {
            let words = freesize_agent::review::parse_words(text);
            self.review_settings = if words.is_empty() {
                review::Settings::default()
            } else {
                review::Settings { words }
            };
            self.review_settings.save();
            self.set_status("Saved the words to look for.");
        }
        self.refresh_home();
    }

    /// The button beside Scan: the path in the box scanned with more
    /// rights. On Windows that's a new window as administrator, and this
    /// one closes; elsewhere a scan in the background that opens here.
    fn scan_as_admin(&mut self) {
        if self.exporting.is_some() || self.scan.is_some() {
            return;
        }
        let Some(ui) = self.ui() else {
            return;
        };
        let typed = ui.get_path().trim().to_string();
        let root = if typed.is_empty() {
            locations::default_path()
        } else {
            PathBuf::from(typed)
        };
        let offer = elevate::offer(freesize_core::access::rights(), &root, false);
        if offer.action.is_empty() {
            return;
        }
        match elevate::start(&offer.action, &root) {
            Ok(elevate::Started::NewWindow) => self.leave_for_admin_window(),
            Ok(elevate::Started::Exporting(exporting)) => {
                self.set_status(&format!(
                    "Scanning {} with more rights in the background; it opens here when done.",
                    root.display()
                ));
                self.exporting = Some(exporting);
                self.ensure_ticking();
            }
            Ok(elevate::Started::Opened) => {
                self.set_status("Turn on Full Disk Access for FreeSize there, then scan again.");
            }
            Err(reason) => self.set_status(&format!("Couldn't scan with more rights: {reason}.")),
        }
    }

    /// The window as administrator scans in this one's place; two windows
    /// of the same scan would only confuse. This one stays open while it's
    /// still moving or deleting something, and closes once that's done.
    fn leave_for_admin_window(&mut self) {
        let busy = self.basket_job.is_some()
            || self.put_back_job.is_some()
            || self.quarantine_job.is_some()
            || self.cleanup_job.is_some()
            || self.junk_clean_job.is_some()
            || self.apk_job.is_some();
        if busy {
            self.set_status(
                "A FreeSize window as administrator is opening. Close this one once it's \
                 done with what it's doing.",
            );
        } else {
            let _ = slint::quit_event_loop();
        }
    }

    fn app_bubbles_resized(&mut self, width: f32, height: f32) {
        if (width, height) != self.app_bubble_size {
            self.app_bubble_size = (width, height);
            self.update_app_bubbles(true);
        }
    }

    /// Fills the start screen's app bubbles. `fresh` starts them over, so
    /// they grow in, as after zooming; otherwise only changes are applied.
    fn update_app_bubbles(&self, fresh: bool) {
        let (width, height) = self.app_bubble_size;
        let (Some(report), Some(view)) = (self.report().filter(|_| self.app_bubbles), self.page)
        else {
            self.app_bubble_rows.clear();
            return;
        };
        if width < 40.0 || height < 40.0 {
            return;
        }
        let items: Vec<BubbleItem> =
            appbubbles::bubbles(report, view, self.app_zoom.as_deref(), width, height)
                .into_iter()
                .map(|b| {
                    let r = b.circle.r;
                    let (color, text_color) = if b.color == u16::MAX {
                        (
                            Color::from_rgb_u8(0x9e, 0xa3, 0xab),
                            Color::from_rgb_u8(0x1b, 0x1b, 0x1f),
                        )
                    } else {
                        block_color(b.color, 0, true)
                    };
                    let (icon, app_icon) = match &b.icon {
                        appbubbles::Icon::App(package) => (0, self.owners.app_icon(package)),
                        appbubbles::Icon::Kind(kind) => (*kind, None),
                    };
                    // App icons are the point here: drawn bigger than kind icons.
                    let labelled = r >= 30.0;
                    let icon_size = match (&app_icon, labelled) {
                        (Some(_), true) => (r * 0.55).min(72.0),
                        (Some(_), false) => r * 1.3,
                        (None, true) => (r * 0.36).min(52.0),
                        (None, false) if r >= 12.0 => r,
                        _ => 0.0,
                    };
                    BubbleItem {
                        id: -1,
                        key: b.key.into(),
                        x: b.circle.x,
                        y: b.circle.y,
                        r,
                        color,
                        text_color,
                        label: if labelled { b.label } else { String::new() }.into(),
                        detail: if r >= 42.0 { b.detail } else { String::new() }.into(),
                        depth: 0,
                        icon,
                        has_app_icon: app_icon.is_some(),
                        app_icon: app_icon.unwrap_or_default(),
                        icon_size,
                        ring: false,
                        pill: false,
                    }
                })
                .collect();
        if fresh {
            self.app_bubble_rows.set_vec(items);
        } else {
            sync_model(&self.app_bubble_rows, items);
        }
    }

    /// A tap on one of the start screen's app bubbles.
    fn app_bubble(&mut self, key: &str) {
        if key == appbubbles::MORE {
            // The smallest apps are easier to read as a list.
            self.app_bubbles = false;
            self.refresh_home();
            return;
        }
        if let Some(part) = key.strip_prefix(appbubbles::PART_PREFIX) {
            let (what, package) = part.split_once(':').unwrap_or((part, ""));
            if what == appbubbles::FILES {
                let folder = self
                    .report()
                    .and_then(|r| r.media.iter().find(|m| m.package == package))
                    .map(|m| m.folder.to_string_lossy().into_owned());
                if let Some(folder) = folder {
                    self.start_scan(folder);
                }
            } else {
                self.open_app(package);
            }
            return;
        }
        self.app_zoom = Some(key.to_string());
        self.refresh_home();
        self.update_app_bubbles(true);
    }

    /// Adds today's size of each drive to the drive history.
    fn record_drives(&mut self) {
        let drives: Vec<_> = locations::list()
            .into_iter()
            .filter_map(|l| Some((l.path.to_string_lossy().into_owned(), disk_space(&l.path)?)))
            .collect();
        self.drive_log = growth::record_drives(&drives, apps::now());
    }

    // ----- Apps -----------------------------------------------------------

    /// Measures every app's storage on a worker thread. A previous result
    /// stays on screen until the new one is in.
    fn start_apps_job(&mut self) {
        if self.apps_job.is_some() {
            return;
        }
        let result: Slot<Result<AppReport, String>> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-apps".into())
            .spawn(move || put(&worker_result, apps::collect()));
        match spawned {
            Ok(_) => {
                self.apps_job = Some(result);
                self.ensure_ticking();
                if !matches!(self.apps, AppsState::Ready(_)) {
                    self.apps = AppsState::Loading;
                }
            }
            Err(err) => self.apps = AppsState::Failed(err.to_string()),
        }
        self.refresh_home();
    }

    fn home_action(&mut self, id: &str) {
        let now = apps::now() as i64;
        match id {
            updater::CHECK => {
                self.updater.check(now);
                self.ensure_ticking();
                self.refresh_home();
            }
            updater::INSTALL => {
                self.updater.install();
                self.ensure_ticking();
                self.set_status("Downloading the release and checking its signature…");
                self.refresh_home();
            }
            updater::NOTES => {
                if let updater::Stage::Found(release) = &self.updater.stage {
                    let _ = crate::actions::open_url(&release.url);
                }
            }
            quarantine::EMPTY_ALL_ACTION => self.confirm_empty(None),
            id if id.starts_with(quarantine::OPEN_PREFIX) => {
                self.quarantine_view =
                    Some(quarantine::View::new(&id[quarantine::OPEN_PREFIX.len()..]));
                self.refresh_quarantine_view();
            }
            id if id.starts_with(quarantine::RESTORE_PREFIX) => {
                self.restore_batch(id[quarantine::RESTORE_PREFIX.len()..].to_string());
            }
            id if id.starts_with(quarantine::EMPTY_PREFIX) => {
                self.confirm_empty(Some(vec![id[quarantine::EMPTY_PREFIX.len()..].to_string()]));
            }
            junk::FIND => {
                self.junk_open(true);
                self.junk_find(false);
            }
            junk::OPEN => {
                self.junk_open(true);
                if phone_junk() {
                    self.junk_from_report();
                } else if !self.junk.looked {
                    self.junk_find(false);
                }
            }
            home::GRANT_USAGE_ACCESS => {
                apps::request_access();
                self.awaiting_usage_access = true;
                self.ensure_ticking();
                // Explain Android's "restricted settings" block in case the
                // switch turns out to be greyed out.
                if matches!(self.apps, AppsState::NeedsAccess { .. }) {
                    self.apps = AppsState::NeedsAccess { asked: true };
                    self.refresh_home();
                }
            }
            home::OPEN_APP_INFO => {
                apps::open_own_settings();
                self.awaiting_usage_access = true;
                self.ensure_ticking();
            }
            home::REFRESH_APPS => self.start_apps_job(),
            home::CLEAR_CACHE => {
                // The cleaner only opens for apps with All files access, and
                // closes unseen otherwise.
                if !storage_access() {
                    self.set_status(
                        "Android's cache cleaner only opens for apps with All files access. \
                         Turn it on for FreeSize, then try again.",
                    );
                    self.grant_permission();
                    return;
                }
                let before = self
                    .report()
                    .map_or(0, |r| home::clearable_cache(r, apps::abilities()));
                if apps::clear_all_cache() {
                    self.cache_check = Some((before, false));
                    // It shows at once, or not at all.
                    self.watch_return_within(3);
                } else {
                    self.set_status(
                        "This phone has no clear-all option, so Storage settings opened instead.",
                    );
                }
            }
            home::GRANT_STORAGE => self.grant_permission(),
            home::TOGGLE_BUBBLES => {
                self.app_bubbles = !self.app_bubbles;
                self.app_zoom = None;
                self.refresh_home();
                self.update_app_bubbles(true);
            }
            home::HUB_CONNECT => self.open_hub_connect(""),
            home::HUB_SETTINGS => self.open_hub_settings(),
            home::HUB_REVIEW => self.show_next_prompt(),
            home::ZOOM_OUT => {
                self.app_zoom = None;
                self.refresh_home();
                self.update_app_bubbles(true);
            }
            home::BACK => self.home_back(),
            _ => {
                if let Some(view) = id.strip_prefix(home::VIEW_PREFIX).and_then(View::from_id) {
                    self.open_page(view);
                } else if let Some(kind) = id
                    .strip_prefix(home::CLEAN_PREFIX)
                    .and_then(JunkKind::from_id)
                {
                    self.request_cleanup(kind);
                } else if let Some(kind) = id
                    .strip_prefix(home::REVIEW_PREFIX)
                    .and_then(ReviewKind::from_id)
                {
                    self.review(kind);
                }
            }
        }
    }

    /// Opens a group of apps as its own page, from the top.
    fn open_page(&mut self, view: View) {
        let Some(ui) = self.ui() else {
            return;
        };
        if self.page.is_none() {
            self.home_scroll = ui.get_home_scroll();
        }
        self.page = Some(view);
        self.app_zoom = None;
        self.refresh_home();
        self.update_app_bubbles(true);
        ui.set_home_scroll(0.0);
    }

    /// Back on a group's page: out of the app the bubbles zoomed into, else
    /// back to the home screen where it was left.
    fn home_back(&mut self) {
        if self.app_zoom.take().is_some() {
            self.refresh_home();
            self.update_app_bubbles(true);
            return;
        }
        if self.page.take().is_some() {
            self.refresh_home();
            if let Some(ui) = self.ui() {
                ui.set_home_scroll(self.home_scroll);
            }
        }
    }

    fn report(&self) -> Option<&AppReport> {
        match &self.apps {
            AppsState::Ready(report) => Some(report),
            _ => None,
        }
    }

    /// Shows the details sheet for an app, or for a removed app.
    fn open_app(&mut self, key: &str) {
        let Some(report) = self.report() else {
            return;
        };
        let (target, content) = if let Some(package) = key.strip_prefix(home::REMOVED_PREFIX) {
            let Some(entry) = report.removed.iter().find(|e| e.package == package) else {
                return;
            };
            (
                SheetTarget::Removed(package.into()),
                home::removed_sheet(entry, report.now),
            )
        } else {
            let Some(app) = report.apps.iter().find(|a| a.package == key) else {
                return;
            };
            (
                SheetTarget::App(key.into()),
                home::app_sheet(app, report, apps::abilities()),
            )
        };
        self.show_sheet(content, Some(target));
    }

    /// Shows the sheet of facts and buttons; `target` is what its buttons
    /// act on.
    fn show_sheet(&mut self, content: home::Sheet, target: Option<SheetTarget>) {
        let Some(ui) = self.ui() else {
            return;
        };
        ui.set_sheet_title(content.title.into());
        ui.set_sheet_subtitle(content.subtitle.into());
        let facts: Vec<SharedString> = content.facts.into_iter().map(Into::into).collect();
        ui.set_sheet_facts(ModelRc::new(VecModel::from(facts)));
        ui.set_sheet_note(content.note.into());
        let buttons: Vec<SheetAction> = content
            .actions
            .into_iter()
            .map(|(id, label, primary)| SheetAction {
                id: id.into(),
                label: label.into(),
                primary,
            })
            .collect();
        ui.set_sheet_actions(ModelRc::new(VecModel::from(buttons)));
        ui.set_sheet_open(true);
        self.sheet = target;
    }

    // ----- FreeSize Hub -------------------------------------------------------

    /// Picks up the Hub's events twice a second. Separate from `timer`, which
    /// stops when nothing runs, so a connected app doesn't wake every 100 ms.
    fn start_hub_timer(&self) {
        if self.hub_timer.running() {
            return;
        }
        let weak = self.me.clone();
        self.hub_timer
            .start(TimerMode::Repeated, Duration::from_millis(500), move || {
                if let Some(rc) = weak.upgrade()
                    && let Ok(mut c) = rc.try_borrow_mut()
                {
                    c.hub_tick();
                }
            });
    }

    fn this_device(&self) -> freesize_proto::DeviceInfo {
        let state = self.hub.state();
        hub::device_info(&device_name(), &os_name(), state.as_ref())
    }

    /// Tells the Hub about this device and its drives.
    fn report_disks(&mut self) {
        if self.hub.state().is_none() {
            return;
        }
        let places: Vec<(String, PathBuf)> = locations::list()
            .into_iter()
            .map(|l| (l.label, l.path))
            .collect();
        self.hub.report(freesize_proto::Report {
            info: Some(self.this_device()),
            disks: Some(freesize_agent::report::disks(&places)),
            ..freesize_proto::Report::default()
        });
        self.disks_reported = Some(std::time::Instant::now());
        self.report_sync();
    }

    /// Tells the Hub what this device keeps of its Nextcloud folders, from
    /// a background thread: each sync folder is walked.
    fn report_sync(&self) {
        let reporter = self.hub.reporter();
        if !reporter.connected() {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("freesize-sync".into())
            .spawn(move || {
                use freesize_agent::nextcloud;
                let accounts = if cfg!(target_os = "android") {
                    nextcloud::android_accounts(&locations::default_path())
                } else {
                    nextcloud::desktop_accounts()
                };
                let report = nextcloud::report(&accounts, apps::now() as i64);
                reporter.send(freesize_proto::Report {
                    sync: Some(report),
                    ..freesize_proto::Report::default()
                });
            });
    }

    /// Android (0.14): with a Hub paired, the phone reports and hears of
    /// requests every 15 minutes while the app is closed, through a job
    /// Android runs; its notices need the notifications permission.
    fn background_on(&self) {
        #[cfg(target_os = "android")]
        if self.hub.state().is_some() {
            crate::android::schedule_hub_job();
            crate::android::request_notifications();
        }
    }

    fn background_off(&self) {
        #[cfg(target_os = "android")]
        crate::android::cancel_hub_job();
    }

    /// A pairing link the PairLink activity left for the app, taken.
    #[cfg(target_os = "android")]
    fn scanned_link(&self) -> Option<String> {
        hub::take_pair_link(&growth::data_dir()?)
    }

    fn hub_tick(&mut self) {
        // A QR code scanned while the app was already running.
        #[cfg(target_os = "android")]
        if let Some(link) = self.scanned_link() {
            self.open_hub_connect(&link);
        }
        let due = self
            .disks_reported
            .is_none_or(|at| at.elapsed() > Duration::from_secs(3600));
        if due && self.hub.state().is_some() {
            if self.disks_reported.is_none() {
                self.background_on();
            }
            self.report_disks();
        }
        let mut home = false;
        for event in self.hub.events() {
            match event {
                hub::Event::Status(status) => {
                    home |= status != self.hub_status;
                    self.hub_status = status;
                }
                hub::Event::Ask(prompt) => {
                    self.prompts.push_back(prompt);
                    self.show_next_prompt();
                    home = true;
                }
                hub::Event::Withdrawn(id) => {
                    self.prompts.retain(|p| p.id != id);
                    if self.prompt.as_ref().is_some_and(|p| p.id == id) {
                        self.prompt = None;
                        if let Some(ui) = self.ui() {
                            ui.set_approval_open(false);
                        }
                        self.set_status("The Hub withdrew its request.");
                        self.show_next_prompt();
                    }
                    home = true;
                }
                hub::Event::Done { text, deleted } => {
                    self.forget_deleted(&deleted);
                    self.set_status(&text);
                    self.quarantine_changed();
                }
                hub::Event::Latest(latest) => {
                    home |= self.updater.hub_said(Some(latest));
                }
                hub::Event::Removed => {
                    self.hub.disconnect();
                    self.background_off();
                    self.prompts.clear();
                    self.hub_status.clear();
                    self.set_status("The Hub removed this device. Connect again to pair anew.");
                    home = true;
                }
                hub::Event::Confirm(hello) => {
                    if let Some(ui) = self.ui() {
                        ui.set_hub_busy(false);
                        ui.set_hub_confirm_fingerprint(hello.key.fingerprint().into());
                        ui.set_hub_connect_status(format!("Reached \"{}\".", hello.name).into());
                    }
                    self.hub_hello = Some(hello);
                }
                hub::Event::Paired(Ok(name)) => {
                    self.hub.connected();
                    self.hub_link = None;
                    self.hub_hello = None;
                    self.hub_status = format!("Connected to {name}");
                    self.background_on();
                    if let Some(ui) = self.ui() {
                        ui.set_hub_busy(false);
                        ui.set_hub_connect_open(false);
                    }
                    self.report_disks();
                    if let Some(report) = self.report() {
                        let apps = hub::apps_report(report);
                        self.hub.report(freesize_proto::Report {
                            apps: Some(apps),
                            ..freesize_proto::Report::default()
                        });
                    }
                    self.set_status(&format!(
                        "Connected to {name}. Scan a drive to send its summary."
                    ));
                    home = true;
                }
                hub::Event::Paired(Err(err)) => {
                    if let Some(ui) = self.ui() {
                        ui.set_hub_busy(false);
                        ui.set_hub_confirm_fingerprint(SharedString::new());
                        ui.set_hub_connect_status(err.into());
                    }
                    self.hub_hello = None;
                }
            }
        }
        if home {
            self.refresh_home();
        }
    }

    fn open_hub_connect(&mut self, link: &str) {
        let Some(ui) = self.ui() else { return };
        ui.set_hub_link(link.into());
        ui.set_hub_code(SharedString::new());
        ui.set_hub_connect_status(SharedString::new());
        ui.set_hub_confirm_fingerprint(SharedString::new());
        ui.set_hub_busy(false);
        ui.set_hub_connect_open(true);
        self.hub_hello = None;
    }

    fn hub_connect(&mut self) {
        let Some(ui) = self.ui() else { return };
        match hub::link_from(&ui.get_hub_link(), &ui.get_hub_code()) {
            Ok(link) => {
                ui.set_hub_busy(true);
                ui.set_hub_connect_status(format!("Reaching {}…", link.hub).into());
                self.hub_link = Some(link.clone());
                self.hub.connect(link, self.this_device(), None);
            }
            Err(err) => ui.set_hub_connect_status(err.into()),
        }
    }

    /// The person compared the Hub's fingerprint and it matches.
    fn hub_confirm(&mut self) {
        let (Some(link), Some(hello)) = (self.hub_link.clone(), self.hub_hello.take()) else {
            return;
        };
        if let Some(ui) = self.ui() {
            ui.set_hub_busy(true);
            ui.set_hub_connect_status("Connecting…".into());
        }
        self.hub.connect(link, self.this_device(), Some(hello));
    }

    fn hub_connect_cancel(&mut self) {
        self.hub_link = None;
        self.hub_hello = None;
        if let Some(ui) = self.ui() {
            ui.set_hub_connect_open(false);
        }
    }

    fn open_hub_settings(&mut self) {
        let Some(state) = self.hub.state() else {
            return;
        };
        let content = home::hub_sheet(&state, &self.hub_status, apps::now() as i64);
        self.show_sheet(content, Some(SheetTarget::Hub));
    }

    /// Shows the next request from the Hub, if none is on screen.
    fn show_next_prompt(&mut self) {
        if self.prompt.is_some() {
            return;
        }
        let Some(prompt) = self.prompts.pop_front() else {
            return;
        };
        let Some(ui) = self.ui() else { return };
        ui.set_approval_title(prompt.title.as_str().into());
        let lines: Vec<SharedString> = prompt.lines.iter().map(|l| l.as_str().into()).collect();
        ui.set_approval_lines(ModelRc::new(VecModel::from(lines)));
        let minutes: Vec<i32> = prompt
            .minutes
            .iter()
            .take(3)
            .map(|&m| i32::try_from(m).unwrap_or(i32::MAX))
            .collect();
        ui.set_approval_minutes(ModelRc::new(VecModel::from(minutes)));
        ui.set_approval_open(true);
        self.prompt = Some(prompt);
        self.refresh_home();
    }

    fn approval_answer(&mut self, answer: i32) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        if let Some(ui) = self.ui() {
            ui.set_approval_open(false);
        }
        let choice = match answer {
            a if a < 0 => freesize_agent::commands::Choice::Deny,
            0 => freesize_agent::commands::Choice::Once,
            minutes => freesize_agent::commands::Choice::For(minutes as u32),
        };
        self.set_status(match choice {
            freesize_agent::commands::Choice::Deny => "Denied. The Hub is told.",
            _ => "Approved. Working on it…",
        });
        self.hub.answer(prompt, choice);
        self.show_next_prompt();
        self.refresh_home();
    }

    /// "Not now": the request waits on the home screen until reviewed.
    fn approval_later(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            self.prompts.push_back(prompt);
        }
        if let Some(ui) = self.ui() {
            ui.set_approval_open(false);
        }
        self.refresh_home();
    }

    // ----- Export -----------------------------------------------------------

    fn open_export(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        let name = if self.focus == tree.root() {
            self.root_label(tree)
        } else {
            tree.name(self.focus).into_owned()
        };
        let content = home::Sheet {
            title: "Export".into(),
            subtitle: format!(
                "{name} · {}",
                format_size(tree.node(self.focus).size_in(self.mode))
            ),
            facts: vec![
                "Report: a page with the biggest folders and files, file types, ages, \
                 clean-up suggestions and changes, to read or share."
                    .into(),
                "Spreadsheet: the biggest folders and files as a CSV table.".into(),
                "Scan file: the whole scan in ncdu's JSON format. Open it again in FreeSize \
                 by typing its path and pressing Scan, or with `ncdu -f`."
                    .into(),
            ],
            note: if cfg!(target_os = "android") {
                "Files are saved to Download/FreeSize.".into()
            } else {
                String::new()
            },
            actions: vec![
                (EXPORT_HTML, "Report (HTML)".into(), true),
                (EXPORT_CSV, "Spreadsheet (CSV)".into(), false),
                (EXPORT_JSON, "Scan file (JSON)".into(), false),
                (sheet::CLOSE, "Close".into(), false),
            ],
        };
        self.show_sheet(content, None);
    }

    /// The file for an export button: its suggested name and contents.
    fn prepare_export(&mut self, kind: &str) -> Option<(String, Vec<u8>)> {
        self.close_sheet();
        let tree = self.tree.as_ref()?;
        let now = apps::now() as i64;
        let label = if self.focus == tree.root() {
            self.root_label(tree)
        } else {
            tree.name(self.focus).into_owned()
        };
        let stem: String = label
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        let name = |ext: &str| format!("{stem}-{}.{ext}", report::iso_date(now));
        match kind {
            EXPORT_HTML => {
                let cleanup = filecleanup::flatten(&self.cleanup_shown);
                let changes: Vec<CleanRow> = self.change_rows.iter().collect();
                let extras = report::Extras {
                    title: &label,
                    now,
                    cleanup: &cleanup,
                    changes: &changes,
                };
                let page = report::html(tree, self.focus, self.mode, &extras);
                Some((name("html"), page.into_bytes()))
            }
            EXPORT_CSV => Some((name("csv"), report::csv(tree, self.focus).into_bytes())),
            EXPORT_JSON => {
                let mut bytes = Vec::new();
                ncdu::export(tree, now, &mut bytes).ok()?;
                Some((name("json"), bytes))
            }
            _ => None,
        }
    }

    fn finish_export(&mut self, saved: Result<Option<PathBuf>, String>) {
        match saved {
            Ok(Some(path)) => {
                self.set_status(&format!("Saved {}", path.display()));
                // On a phone, show the report right away.
                if cfg!(target_os = "android") && path.extension().is_some_and(|e| e == "html") {
                    match actions::open(&path) {
                        Ok(actions::Open::TryAgain) => {
                            self.pending_open = Some((path, 0));
                            self.ensure_ticking();
                        }
                        Ok(actions::Open::Done) | Err(_) => {}
                    }
                }
            }
            Ok(None) => {}
            Err(err) => self.set_status(&format!("Could not save: {err}")),
        }
    }

    fn close_sheet(&mut self) {
        self.sheet = None;
        if let Some(ui) = self.ui() {
            ui.set_sheet_open(false);
        }
    }

    fn sheet_action(&mut self, id: &str) {
        let target = self.sheet.take();
        self.close_sheet();
        let Some(target) = target else {
            return;
        };
        let demo = !cfg!(target_os = "android");
        match (id, &target) {
            (sheet::CLOSE, _) => {}
            (sheet::UNINSTALL, SheetTarget::App(package)) => {
                apps::uninstall(package);
                self.watch_return();
                if demo {
                    self.set_status(&format!("Uninstall {package}: only on Android."));
                }
            }
            (sheet::ARCHIVE, SheetTarget::App(package)) => {
                apps::open(package);
                self.watch_return();
                self.set_status("On the app's info page, tap Archive.");
            }
            (sheet::APP_INFO, SheetTarget::App(package)) => {
                apps::open(package);
                self.watch_return();
            }
            (sheet::STORE, SheetTarget::App(package) | SheetTarget::Removed(package)) => {
                let installer = self.installer_of(&target);
                apps::open_in_store(package, installer.as_deref());
            }
            (sheet::SEARCH_STORE, SheetTarget::Removed(package)) => {
                let label = self
                    .report()
                    .and_then(|r| r.removed.iter().find(|e| &e.package == package))
                    .map_or_else(|| package.clone(), |e| e.label.clone());
                apps::search_store(&label);
            }
            (sheet::SAVE_APK, SheetTarget::App(package)) => self.save_apk(package),
            (sheet::BROWSE, SheetTarget::App(package)) => {
                let folder = self
                    .report()
                    .and_then(|r| r.media.iter().find(|m| &m.package == package))
                    .map(|m| m.folder.to_string_lossy().into_owned());
                if let Some(folder) = folder {
                    self.start_scan(folder);
                }
            }
            (id, SheetTarget::Review(kind)) => {
                let place = REVIEW_IDS.iter().position(|&r| r == id).and_then(|i| {
                    self.report()?
                        .reviews
                        .iter()
                        .find(|r| r.kind == *kind)?
                        .places
                        .get(i)
                        .cloned()
                });
                if let Some(place) = place {
                    self.open_place(*kind, &place);
                }
            }
            (sheet::HUB_REVOKE, SheetTarget::Hub) => {
                self.hub.revoke_grants();
                self.set_status("Grants ended: the Hub's requests ask first again.");
            }
            (sheet::HUB_DISCONNECT, SheetTarget::Hub) => {
                self.hub.disconnect();
                self.background_off();
                self.hub_status.clear();
                self.prompts.clear();
                self.set_status("Disconnected. An admin can remove this device from the Hub.");
                self.refresh_home();
            }
            (sheet::FORGET, SheetTarget::Removed(package)) => {
                if let Err(err) = apps::forget(package) {
                    self.set_status(&format!("Couldn't update the app history: {err}"));
                }
                if let AppsState::Ready(report) = &mut self.apps {
                    report.removed.retain(|e| &e.package != package);
                }
                self.refresh_home();
            }
            _ => {}
        }
    }

    fn installer_of(&self, target: &SheetTarget) -> Option<String> {
        let report = self.report()?;
        match target {
            SheetTarget::App(package) => report
                .apps
                .iter()
                .find(|a| &a.package == package)?
                .installer
                .clone(),
            SheetTarget::Removed(package) => report
                .removed
                .iter()
                .find(|e| &e.package == package)?
                .installer
                .clone(),
            SheetTarget::Review(_) | SheetTarget::Hub => None,
        }
    }

    /// Copies an app's APK to Download/FreeSize on a worker thread.
    fn save_apk(&mut self, package: &str) {
        if self.apk_job.is_some() {
            self.set_status("Still saving the previous APK…");
            return;
        }
        if !storage_access() {
            self.grant_permission();
            self.set_status("Saving an APK needs All files access. Allow it, then try again.");
            return;
        }
        let Some(app) = self
            .report()
            .and_then(|r| r.apps.iter().find(|a| a.package == package))
        else {
            return;
        };
        let (package, label, version) =
            (app.package.clone(), app.label.clone(), app.version.clone());
        let saving = format!("Saving the APK of {label}…");
        let result: Slot<Result<String, String>> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-apk".into())
            .spawn(move || put(&worker_result, apps::save_apk(&package, &label, &version)));
        match spawned {
            Ok(_) => {
                self.apk_job = Some(result);
                self.ensure_ticking();
                self.set_status(&saving);
            }
            Err(err) => self.set_status(&format!("Couldn't save the APK: {err}")),
        }
    }

    /// Opens old screenshots or chat media to look through: the folder, or a
    /// sheet to pick one when there are several.
    fn review(&mut self, kind: ReviewKind) {
        let Some(review) = self
            .report()
            .and_then(|r| r.reviews.iter().find(|r| r.kind == kind))
            .cloned()
        else {
            return;
        };
        match review.places.as_slice() {
            [] => {}
            [place] => self.open_place(kind, place),
            _ => self.show_sheet(
                home::review_sheet(&review, &REVIEW_IDS),
                Some(SheetTarget::Review(kind)),
            ),
        }
    }

    /// Scans a folder to review, showing its files right away: only the old
    /// ones for screenshots.
    fn open_place(&mut self, kind: ReviewKind, place: &Place) {
        if self.scan.is_some() {
            return;
        }
        self.pending_files = Some(match kind {
            ReviewKind::Screenshots => Some(FileFilter::OlderThan(
                apps::now() as i64 - cleanup::OLD_SCREENSHOT,
            )),
            ReviewKind::ChatMedia => None,
        });
        self.start_scan(place.folder.to_string_lossy().into_owned());
    }

    /// Asks before deleting a kind of junk, with the shared confirmation
    /// dialog.
    fn request_cleanup(&mut self, kind: JunkKind) {
        if self.cleanup_job.is_some() {
            self.set_status("Still deleting…");
            return;
        }
        let Some(junk) = self
            .report()
            .and_then(|r| r.junk.iter().find(|j| j.kind == kind))
        else {
            return;
        };
        let text = home::junk_confirm(junk);
        let Some(ui) = self.ui() else {
            return;
        };
        ui.set_confirm_text(text.into());
        ui.set_delete_label("Delete".into());
        ui.set_confirm_open(true);
        self.pending_cleanup = Some(kind);
    }

    /// Deletes the junk the user confirmed, on a worker thread.
    fn run_cleanup(&mut self, kind: JunkKind) {
        let Some(junk) = self
            .report()
            .and_then(|r| r.junk.iter().find(|j| j.kind == kind))
            .cloned()
        else {
            return;
        };
        let result: Slot<(u64, Vec<String>)> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-cleanup".into())
            .spawn(move || put(&worker_result, apps::delete_junk(&junk)));
        match spawned {
            Ok(_) => {
                self.cleanup_job = Some(result);
                self.ensure_ticking();
                self.set_status("Deleting…");
            }
            Err(err) => self.set_status(&format!("Couldn't delete: {err}")),
        }
    }

    fn watch_return(&mut self) {
        self.watch_return_within(30);
    }

    /// Notices the user coming back from a system screen, giving up if it
    /// hasn't opened within `seconds`.
    fn watch_return_within(&mut self, seconds: u64) {
        self.return_watch = Some(ReturnWatch {
            left: false,
            ticks: 0,
            patience: seconds,
        });
        self.ensure_ticking();
    }

    fn set_status(&self, text: &str) {
        if let Some(ui) = self.ui() {
            ui.set_status(text.into());
        }
    }

    fn refresh_status(&self) {
        let (Some(ui), Some(tree), Some(summary)) = (self.ui(), &self.tree, &self.summary) else {
            return;
        };
        let root = tree.node(tree.root());
        let mut status = format!(
            "{} in {} and {} · {}",
            format_size(root.size_in(self.mode)),
            plural(u64::from(root.files), "file", "files"),
            plural(u64::from(root.dirs), "folder", "folders"),
            if self.imported {
                "a saved scan, so nothing can be opened or deleted".to_string()
            } else {
                format!("scanned in {:.1}s", summary.seconds)
            }
        );
        if summary.errors > 0 {
            status.push_str(&format!(
                " · {} could not be read, see Clean up",
                format_count(summary.errors)
            ));
        }
        if summary.cancelled {
            status = format!("Scan cancelled, showing partial results · {status}");
        }
        ui.set_status(status.into());
    }

    // ----- Tree and selection ---------------------------------------------

    fn toggle_row(&mut self, row: i32) {
        if let (Some(tree), Ok(row)) = (&self.tree, usize::try_from(row)) {
            self.rows.toggle(tree, self.mode, row);
        }
    }

    fn select(&mut self, id: i32, reveal: bool) {
        let Some(tree) = &self.tree else {
            return;
        };
        let Ok(id) = NodeId::try_from(id) else {
            return;
        };
        if id as usize >= tree.len() {
            return;
        }
        let node = tree.node(id);
        // Selecting a folder zooms the treemap into it; selecting a file keeps
        // the current view unless the file is outside it.
        let focus = if node.is_dir() {
            id
        } else if tree.ancestors(id).any(|a| a == self.focus) {
            self.focus
        } else if node.parent != NO_NODE {
            node.parent
        } else {
            id
        };
        self.selected = id;
        if reveal {
            self.reveal_in_tree(id);
        }
        if focus != self.focus {
            self.focus = focus;
            self.refresh_focus_views();
        }
        self.refresh_selection();
    }

    /// Expands the folders above `id` and scrolls its row into view.
    fn reveal_in_tree(&self, id: NodeId) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let mut chain: Vec<NodeId> = tree.ancestors(id).skip(1).collect();
        chain.reverse();
        let mut row = 0;
        for folder in chain {
            let Some(found) = self.rows.position(folder, row) else {
                return;
            };
            row = found;
            if self.rows.entry(row).is_some_and(|e| !e.expanded) {
                self.rows.toggle(tree, self.mode, row);
            }
        }
        let target = self.rows.position(id, row).unwrap_or(row);
        let row_height = ui.get_tree_row_height();
        ui.set_tree_scroll(-((target as f32 - 3.0).max(0.0) * row_height));
    }

    fn go_up(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        let parent = tree.node(self.focus).parent;
        if parent != NO_NODE {
            self.select(parent as i32, true);
        }
    }

    fn size_mode_changed(&mut self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        self.mode = if ui.get_size_on_disk() {
            SizeMode::Allocated
        } else {
            SizeMode::Logical
        };
        let expanded = self.rows.expanded_nodes();
        self.rows.rebuild(tree, self.mode, &expanded);
        self.refresh_focus_views();
        self.refresh_selection();
        self.refresh_status();
        self.refresh_cleanup();
    }

    fn refresh_selection(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let id = self.selected;
        let node = tree.node(id);
        let title = if id == tree.root() {
            tree.path(id).to_string_lossy().into_owned()
        } else {
            tree.name(id).into_owned()
        };

        let mut parts = vec![match self.mode {
            SizeMode::Allocated => format!(
                "{} on disk ({} file size)",
                format_size(node.allocated),
                format_size(node.size)
            ),
            SizeMode::Logical => format!(
                "{} ({} on disk)",
                format_size(node.size),
                format_size(node.allocated)
            ),
        }];
        if node.is_dir() {
            parts.push(format!(
                "{}, {}",
                plural(u64::from(node.files), "file", "files"),
                plural(u64::from(node.dirs), "folder", "folders")
            ));
        }
        if let Some(date) = format_date(node.modified) {
            parts.push(format!("modified {date}"));
        }
        let note = rows::note(node.flags);
        if !note.is_empty() {
            parts.push(note.to_string());
        }

        ui.set_selected_id(id as i32);
        ui.set_selected_title(title.into());
        ui.set_selected_detail(parts.join(" · ").into());
        ui.set_can_delete(id != tree.root() && self.deletion.is_none() && !self.imported);
        // Android has no file manager to open a folder in.
        ui.set_can_open(!self.imported && !(cfg!(target_os = "android") && node.is_dir()));
        ui.set_in_basket(self.basket.contains(&id));
        ui.set_can_review(node.is_dir() && node.files > 0);
        ui.set_can_go_up(self.focus != tree.root());
    }

    // ----- Clean up tab ---------------------------------------------------

    // ----- Changes tab -----------------------------------------------------

    /// Picks the earlier scan to compare with; `index` counts from the
    /// newest, as the picker lists them.
    fn compare_with(&mut self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        self.compare = self.earlier.len().checked_sub(index + 1);
        self.refresh_changes();
        self.update_map();
    }

    fn refresh_changes(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let now = apps::now() as i64;
        let labels: Vec<SharedString> = self
            .earlier
            .iter()
            .rev()
            .map(|s| {
                format!(
                    "{} · {}",
                    changes::when(now, s.taken),
                    format_size(s.total(self.mode))
                )
                .into()
            })
            .collect();
        ui.set_change_scans(ModelRc::new(VecModel::from(labels)));
        if let Some(i) = self.compare {
            ui.set_change_scan_index((self.earlier.len() - 1 - i) as i32);
        }
        let chart = changes::chart(
            &self.earlier,
            tree.node(tree.root()).size_in(self.mode),
            now,
            self.mode,
            self.compare,
            self.chart_size,
        )
        .unwrap_or_default();
        ui.set_change_chart_line(chart.line.into());
        ui.set_change_chart_points(ModelRc::new(VecModel::from(chart.points)));
        ui.set_change_chart_high(chart.high.into());
        ui.set_change_chart_low(chart.low.into());
        ui.set_change_chart_start(chart.start.into());
        ui.set_change_chart_end(chart.end.into());
        let rows = match self.compare.and_then(|i| self.earlier.get(i)) {
            Some(earlier) => changes::rows(tree, &self.root_label(tree), earlier, self.mode, now),
            None => vec![CleanRow {
                kind: 0,
                id: -1,
                title: "No earlier scan of this folder".into(),
                subtitle: "FreeSize keeps the folder sizes of every scan. Scan this folder again \
                           another day to see what grew, what's new and what's gone."
                    .into(),
                ..Default::default()
            }],
        };
        sync_model(&self.change_rows, rows);
    }

    /// Sizes of the compared scan's folders by path, for growth on bubbles.
    fn compare_sizes(&self) -> HashMap<&str, u64> {
        self.compare
            .and_then(|i| self.earlier.get(i))
            .map(|s| s.sizes(self.mode))
            .unwrap_or_default()
    }

    /// What the scanned folder is called: its drive's name, or its own.
    fn root_label(&self, tree: &Tree) -> String {
        let root_path = tree.path(tree.root());
        locations::list()
            .into_iter()
            .find(|l| l.path == root_path)
            .map(|l| l.label)
            .or_else(|| {
                root_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| root_path.display().to_string())
    }

    /// Finds rebuildable folders and old files again, then shows the tab.
    fn refresh_cleanup(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        // System caches, except inside folders already listed as rebuildable.
        let ignore_case = cfg!(windows) || cfg!(target_os = "macos");
        let spots: Vec<Spot> = system_spots(tree, &default_spot_rules(), ignore_case)
            .into_iter()
            .filter(|s| {
                !tree
                    .ancestors(s.node)
                    .any(|a| self.rebuildable.contains(&a))
            })
            .collect();
        let offer = elevate::offer(
            freesize_core::access::rights(),
            &tree.path(tree.root()),
            self.imported,
        );
        self.cleanup_found = filecleanup::found_sections(
            tree,
            self.mode,
            apps::now() as i64,
            &spots,
            self.hidden.as_ref(),
            &offer,
        );
        self.quarantine = quarantine::list();
        self.show_cleanup();
        self.refresh_changes();
    }

    fn show_cleanup(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        let duplicates = match &self.dupes {
            DupeSearch::Idle => Duplicates::NotSearched,
            DupeSearch::Running { progress, .. } => Duplicates::Searching {
                done: progress.done.load(Ordering::Relaxed),
                files: progress.files.load(Ordering::Relaxed),
            },
            DupeSearch::Done(groups) => Duplicates::Found(groups),
        };
        let mut sections: Vec<filecleanup::Section> = filecleanup::quarantine_section(
            quarantine::rows(&self.quarantine, apps::now() as i64),
            self.quarantine.iter().map(|b| b.bytes).sum(),
        )
        .into_iter()
        .collect();
        if let Some(review) = &mut self.review {
            if tree.node(review.folder).has(freesize_core::flags::DELETED)
                || tree
                    .ancestors(review.folder)
                    .any(|a| tree.node(a).has(freesize_core::flags::DELETED))
            {
                self.review = None;
                self.review_words = None;
                self.review_copies = None;
                self.review_previews = None;
            } else {
                let mut page = review.page(tree, self.mode, &tree.path(tree.root()));
                review::show_running(
                    &mut page,
                    review::Running {
                        words: self.review_words.as_ref().map(ReviewJob::running),
                        copies: self.review_copies.as_ref().map(ReviewJob::running),
                    },
                );
                sections.insert(0, page);
            }
        }
        sections.extend(self.cleanup_found.iter().cloned());
        let mut dupes = filecleanup::duplicate_section(
            tree,
            self.mode,
            duplicates,
            self.dupe_filter.as_deref(),
        );
        if !dupes
            .filters
            .iter()
            .any(|(ext, _)| Some(ext) == self.dupe_filter.as_ref())
        {
            // The chosen type is gone (deleted, or a new search).
            if self.dupe_filter.take().is_some() {
                dupes = filecleanup::duplicate_section(
                    tree,
                    self.mode,
                    match &self.dupes {
                        DupeSearch::Done(groups) => Duplicates::Found(groups),
                        _ => Duplicates::NotSearched,
                    },
                    None,
                );
            }
        }
        sections.push(dupes);
        let lookup;
        let photos = match &self.photos {
            PhotoSearch::Idle => Similar::NotSearched,
            PhotoSearch::Running { progress, .. } => Similar::Searching {
                done: progress.done.load(Ordering::Relaxed),
                files: progress.files.load(Ordering::Relaxed),
            },
            PhotoSearch::Done { groups, previews } => {
                lookup = |id: NodeId| previews.get(&id).cloned();
                Similar::Found(groups, &lookup)
            }
        };
        sections.push(filecleanup::similar_section(tree, self.mode, photos));
        if self.imported {
            // A read-only scan: nothing to tick.
            for section in &mut sections {
                section.picks.clear();
                section.pick_label = "";
                for row in &mut section.rows {
                    row.checkable = false;
                    if row.kind == 2 {
                        row.action = "".into();
                    }
                }
            }
        }
        if !sections
            .iter()
            .any(|s| s.key == self.cleanup_page && s.has_page())
        {
            self.cleanup_page.clear();
        }
        self.cleanup_shown = sections;

        let Some(ui) = self.ui() else {
            return;
        };
        let page = self
            .cleanup_shown
            .iter()
            .find(|s| s.key == self.cleanup_page);
        sync_model(
            &self.cleanup_rows,
            page.map(|s| s.rows.clone()).unwrap_or_default(),
        );
        ui.set_cleanup_page(self.cleanup_page.as_str().into());
        ui.set_cleanup_summary(
            if self.imported {
                "This scan is open read-only, so nothing can be selected here. Each card says \
                 what was found."
            } else {
                "Open a card to look through what was found and tick what should go. Ticked \
                 items are listed once more before anything happens; then they're moved to the \
                 quarantine, where Put back returns them until you empty it."
            }
            .into(),
        );
        if let Some(page) = page {
            ui.set_cleanup_page_title(page.title.as_str().into());
            ui.set_cleanup_page_blurb(page.blurb.as_str().into());
            let bytes = page.shown_bytes.unwrap_or(page.bytes);
            ui.set_cleanup_page_size(if bytes > 0 && page.key != filecleanup::QUARANTINE {
                format!("{} to free", format_size(bytes)).into()
            } else {
                "".into()
            });
            ui.set_cleanup_page_action(page.action.as_str().into());
            ui.set_cleanup_page_action_2(page.action2.as_str().into());
            ui.set_cleanup_page_warn(page.warn);
            ui.set_cleanup_pick_label(if page.picks.is_empty() {
                "".into()
            } else {
                page.pick_label.into()
            });
            let mut filters: Vec<SharedString> = Vec::new();
            if !page.filters.is_empty() && page.filter_index.is_none() {
                filters.push("Every type".into());
            }
            filters.extend(page.filters.iter().map(|(_, label)| label.as_str().into()));
            let index = page.filter_index.unwrap_or_else(|| {
                self.dupe_filter
                    .as_ref()
                    .and_then(|f| page.filters.iter().position(|(ext, _)| ext == f))
                    .map_or(0, |i| i + 1)
            });
            ui.set_cleanup_filters(ModelRc::new(VecModel::from(filters)));
            ui.set_cleanup_filter_index(index as i32);
        }
        self.mark_cleanup();
    }

    /// Ticks what's in the basket on the Clean up tab, and counts it on
    /// the cards. Cheap enough to run on every change to the basket.
    fn mark_cleanup(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let basket: HashSet<NodeId> = self.basket.iter().copied().collect();
        let covered = |id: NodeId| tree.ancestors(id).any(|a| basket.contains(&a));
        let picked = |rows: &[CleanRow]| -> (u64, u64) {
            rows.iter()
                .filter(|r| r.kind == 1 && r.checkable && covered(r.id as NodeId))
                .fold((0, 0), |(n, bytes), r| {
                    (n + 1, bytes + tree.node(r.id as NodeId).size_in(self.mode))
                })
        };
        let cards: Vec<CleanSection> = self
            .cleanup_shown
            .iter()
            .map(|s| {
                let (n, _) = picked(&s.rows);
                CleanSection {
                    key: s.key.into(),
                    title: s.title.as_str().into(),
                    // A card without a page says what it would do instead.
                    note: if s.has_page() || s.blurb.is_empty() || s.blurb == s.note {
                        s.note.as_str().into()
                    } else {
                        format!("{} · {}", s.note, s.blurb).into()
                    },
                    size: if s.bytes > 0 && s.key != filecleanup::QUARANTINE {
                        format_size(s.bytes).into()
                    } else {
                        "".into()
                    },
                    picked: if n > 0 {
                        format!("{n} selected").into()
                    } else {
                        "".into()
                    },
                    action: s.action.as_str().into(),
                    has_page: s.has_page(),
                    warn: s.warn,
                }
            })
            .collect();
        sync_model(&self.cleanup_sections, cards);

        let page = self
            .cleanup_shown
            .iter()
            .find(|s| s.key == self.cleanup_page);
        let (n, bytes) = page.map_or((0, 0), |s| picked(&s.rows));
        ui.set_cleanup_any_picked(n > 0);
        ui.set_cleanup_page_picked(if n > 0 {
            format!(
                "{} selected here ({}). Review them in the bar below.",
                plural(n, "item", "items"),
                format_size(bytes)
            )
            .into()
        } else {
            "".into()
        });

        // Rows: ticks, and a warning on a group whose every copy is ticked.
        let count = self.cleanup_rows.row_count();
        let mut group: Option<(usize, bool, usize)> = None; // (row, all ticked, items)
        let finish_group = |group: Option<(usize, bool, usize)>| {
            let Some((at, all, items)) = group else {
                return;
            };
            let Some(mut row) = self.cleanup_rows.row_data(at) else {
                return;
            };
            if row.id < 0 {
                return;
            }
            let extras = page
                .and_then(|p| p.groups.get(row.id as usize))
                .map(|g| g.iter().all(|&id| covered(id)) && !g.is_empty())
                .unwrap_or(false);
            let warn = all && items > 1 && !row.action.is_empty();
            let similar = page.is_some_and(|p| p.key == filecleanup::SIMILAR);
            let mut changed = row.clone();
            changed.warn = warn;
            changed.subtitle = if warn {
                if similar {
                    "Every photo of this set is selected: none of them would be left."
                } else {
                    "Every copy is selected: none would be left."
                }
            } else {
                ""
            }
            .into();
            if !row.action.is_empty() {
                changed.action = if extras {
                    filecleanup::UNSELECT
                } else if similar {
                    filecleanup::SELECT_SMALLER
                } else {
                    filecleanup::SELECT_COPIES
                }
                .into();
            }
            if changed != row {
                row = changed;
                self.cleanup_rows.set_row_data(at, row);
            }
        };
        for i in 0..count {
            let Some(row) = self.cleanup_rows.row_data(i) else {
                continue;
            };
            match row.kind {
                2 => {
                    finish_group(group.take());
                    group = Some((i, true, 0));
                }
                1 => {
                    let checked = row.checkable && covered(row.id as NodeId);
                    if let Some((_, all, items)) = &mut group {
                        *all &= checked;
                        *items += 1;
                    }
                    if row.checked != checked {
                        let mut row = row;
                        row.checked = checked;
                        self.cleanup_rows.set_row_data(i, row);
                    }
                }
                _ => finish_group(group.take()),
            }
        }
        finish_group(group);

        // Similar photos are shown as sets of tiles.
        let mut sets: Vec<PhotoSet> = Vec::new();
        let review_photos = self.cleanup_page == filecleanup::REVIEW
            && self
                .review
                .as_ref()
                .is_some_and(|r| r.tab == review::PHOTOS);
        if self.cleanup_page == filecleanup::SIMILAR || review_photos {
            let mut photos: Vec<CleanRow> = Vec::new();
            let flush = |sets: &mut Vec<PhotoSet>, photos: &mut Vec<CleanRow>| {
                if let Some(set) = sets.last_mut() {
                    set.photos = ModelRc::new(VecModel::from(std::mem::take(photos)));
                }
            };
            for row in self.cleanup_rows.iter() {
                match row.kind {
                    2 => {
                        flush(&mut sets, &mut photos);
                        sets.push(PhotoSet {
                            index: row.id,
                            title: row.title.clone(),
                            warning: if row.warn {
                                row.subtitle.clone()
                            } else {
                                "".into()
                            },
                            action: row.action.clone(),
                            photos: ModelRc::default(),
                        });
                    }
                    1 => photos.push(row),
                    _ => {}
                }
            }
            flush(&mut sets, &mut photos);
        }
        ui.set_cleanup_photo_sets(ModelRc::new(VecModel::from(sets)));
    }

    /// A Clean up button, by section key; a quarantine batch's buttons
    /// carry its id.
    fn cleanup_action(&mut self, key: &str) {
        if key == filecleanup::REVIEW {
            self.review_compare();
        } else if key == "review:2" {
            self.review_look_inside();
        } else if key == filecleanup::SIMILAR {
            self.photo_action();
        } else if key == filecleanup::DUPLICATES {
            self.duplicate_action();
        } else if key == filecleanup::UNREADABLE {
            self.fuller_scan();
        } else if key == filecleanup::QUARANTINE {
            self.confirm_empty(None);
        } else if let Some(id) = key.strip_prefix(quarantine::RESTORE_PREFIX) {
            self.restore_batch(id.to_string());
        } else if let Some(id) = key.strip_prefix(quarantine::EMPTY_PREFIX) {
            self.confirm_empty(Some(vec![id.to_string()]));
        }
    }

    /// Opens a section's page, or the overview for "".
    fn cleanup_open(&mut self, key: &str) {
        self.cleanup_page = key.to_string();
        self.show_cleanup();
    }

    /// Double-clicking a row: open the file to look at it.
    fn cleanup_open_item(&mut self, id: i32) {
        self.select(id, true);
        self.open_selected();
    }

    fn cleanup_check(&mut self, id: i32) {
        if self.imported || id < 0 {
            return;
        }
        self.toggle_basket(id as NodeId);
    }

    /// The open page's section.
    fn cleanup_section(&self) -> Option<&filecleanup::Section> {
        self.cleanup_shown
            .iter()
            .find(|s| s.key == self.cleanup_page)
    }

    /// A group's button: tick every copy or photo but the kept one, or
    /// untick them again.
    fn cleanup_group(&mut self, index: i32) {
        if self.imported {
            return;
        }
        let Some(extras) = self
            .cleanup_section()
            .and_then(|s| s.groups.get(usize::try_from(index).ok()?))
            .cloned()
        else {
            return;
        };
        let Some(tree) = &self.tree else {
            return;
        };
        let all = extras
            .iter()
            .all(|&id| tree.ancestors(id).any(|a| self.basket.contains(&a)));
        if all {
            self.basket.retain(|b| !extras.contains(b));
            self.refresh_basket();
        } else {
            self.add_to_basket(&extras);
        }
    }

    /// The page's "select all" (`true`) or "clear" (`false`).
    fn cleanup_pick(&mut self, all: bool) {
        if self.imported {
            return;
        }
        let Some(section) = self.cleanup_section() else {
            return;
        };
        if all {
            let picks = section.picks.clone();
            self.add_to_basket(&picks);
        } else {
            let listed: HashSet<NodeId> = section
                .rows
                .iter()
                .filter(|r| r.kind == 1 && r.checkable)
                .map(|r| r.id as NodeId)
                .collect();
            self.basket.retain(|b| !listed.contains(b));
            self.refresh_basket();
        }
    }

    fn cleanup_filter(&mut self, index: i32) {
        if self.cleanup_page == filecleanup::REVIEW {
            if let Some(review) = &mut self.review {
                review.tab = usize::try_from(index)
                    .unwrap_or(0)
                    .min(review::TABS.len() - 1);
            }
            self.show_cleanup();
            return;
        }
        let filter = usize::try_from(index - 1).ok().and_then(|i| {
            self.cleanup_section()
                .and_then(|s| s.filters.get(i))
                .map(|(ext, _)| ext.clone())
        });
        self.dupe_filter = filter;
        self.show_cleanup();
    }

    // ----- Reviewing a folder ---------------------------------------------

    /// "Review this folder": opens its page on the Clean up tab, and starts
    /// making previews of its photos.
    fn review_folder(&mut self, id: i32) {
        let Some(tree) = &self.tree else {
            return;
        };
        let Ok(id) = NodeId::try_from(id) else {
            return;
        };
        if (id as usize) >= tree.len() || !tree.node(id).is_dir() {
            return;
        }
        if self.review.as_ref().is_none_or(|r| r.folder != id) {
            self.review_words = None;
            self.review_copies = None;
            self.review_previews = None;
            self.review = Some(review::Review::new(id));
            let photos: Vec<(NodeId, PathBuf)> = review::photos(tree, id, self.mode)
                .into_iter()
                .filter(|&p| !tree.node(p).has(freesize_core::flags::CLOUD))
                .take(review::MAX_PHOTOS)
                .map(|p| (p, tree.path(p)))
                .collect();
            if !photos.is_empty() {
                let job = ReviewJob {
                    progress: Arc::new(DuplicateProgress::new()),
                    result: Arc::default(),
                };
                job.progress
                    .files
                    .store(photos.len() as u64, Ordering::Relaxed);
                let (progress, result) = (job.progress.clone(), job.result.clone());
                let spawned = std::thread::Builder::new()
                    .name("freesize-previews".into())
                    .spawn(move || {
                        for (id, path) in photos {
                            if progress.is_cancelled() {
                                break;
                            }
                            let preview = similar::preview(&path);
                            result
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get_or_insert_with(Vec::new)
                                .push((id, preview));
                            progress.done.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                if spawned.is_ok() {
                    self.review_previews = Some(job);
                }
            }
        }
        self.cleanup_page = filecleanup::REVIEW.into();
        if let Some(ui) = self.ui() {
            // The Clean up tab.
            ui.set_wide_tab(3);
            ui.set_compact_tab(4);
        }
        self.show_cleanup();
        self.ensure_ticking();
    }

    /// Stops the review's searches and forgets it.
    fn close_review(&mut self) {
        self.review_words = None;
        self.review_copies = None;
        self.review_previews = None;
        self.review = None;
    }

    /// "Compare with my current files": the folder's files by content
    /// against the rest of the scan; or stops it.
    fn review_compare(&mut self) {
        if self.review_copies.take().is_some() {
            self.show_cleanup();
            return;
        }
        let (Some(tree), Some(review)) = (&self.tree, &self.review) else {
            return;
        };
        let candidates = freesize_core::review::copy_candidates(tree, review.folder);
        let job = ReviewJob {
            progress: Arc::new(DuplicateProgress::new()),
            result: Arc::default(),
        };
        let (progress, result) = (job.progress.clone(), job.result.clone());
        let spawned = std::thread::Builder::new()
            .name("freesize-compare".into())
            .spawn(move || {
                let any = candidates.files() > 0;
                put(&result, (any, candidates.find(&progress)));
            });
        match spawned {
            Ok(_) => self.review_copies = Some(job),
            Err(err) => self.set_status(&format!("Could not start comparing: {err}")),
        }
        self.show_cleanup();
        self.ensure_ticking();
    }

    /// "Look inside files": the words from Settings in the folder's text,
    /// Office and PDF files; or stops it.
    fn review_look_inside(&mut self) {
        if self.review_words.take().is_some() {
            self.show_cleanup();
            return;
        }
        let (Some(tree), Some(review)) = (&self.tree, &mut self.review) else {
            return;
        };
        let files = review::searchable(tree, review.folder, self.mode);
        let words = self.review_settings.words.clone();
        review.looked_for = words.clone();
        let job = ReviewJob {
            progress: Arc::new(DuplicateProgress::new()),
            result: Arc::default(),
        };
        job.progress
            .files
            .store(files.len() as u64, Ordering::Relaxed);
        let (progress, result) = (job.progress.clone(), job.result.clone());
        let spawned = std::thread::Builder::new()
            .name("freesize-words".into())
            .spawn(move || {
                let mut found = HashMap::new();
                for (id, path) in files {
                    if progress.is_cancelled() {
                        return;
                    }
                    let words = freesize_agent::review::words_in(&path, &words);
                    if !words.is_empty() {
                        found.insert(id, words);
                    }
                    progress.done.fetch_add(1, Ordering::Relaxed);
                }
                put(&result, found);
            });
        match spawned {
            Ok(_) => self.review_words = Some(job),
            Err(err) => self.set_status(&format!("Could not start looking inside files: {err}")),
        }
        self.show_cleanup();
        self.ensure_ticking();
    }

    /// Picks up the review's results and previews.
    fn tick_review(&mut self) {
        let mut changed = false;
        if let Some(job) = &self.review_words
            && let Some(found) = take(&job.result)
        {
            self.review_words = None;
            if let Some(review) = &mut self.review {
                review.words = Some(found);
            }
            changed = true;
        }
        if let Some(job) = &self.review_copies
            && let Some((any, copies)) = take(&job.result)
        {
            self.review_copies = None;
            if let Some(review) = &mut self.review {
                review.copies = Some(copies);
                review.compared_any = any;
            }
            changed = true;
        }
        if let Some(job) = &self.review_previews {
            let (done, of) = job.running();
            let made = take(&job.result).unwrap_or_default();
            if done >= of || job.progress.is_cancelled() {
                self.review_previews = None;
            }
            if let Some(review) = &mut self.review {
                for (id, pixels) in made {
                    if let Some(pixels) = pixels {
                        review.previews.insert(id, pixels.image());
                    }
                }
            }
            // Previews show in batches, not one by one.
            if self.review_previews.is_none() || self.ticks.is_multiple_of(10) {
                changed |= self.cleanup_page == filecleanup::REVIEW;
            }
        }
        let searching = self.review_words.is_some() || self.review_copies.is_some();
        if changed || (searching && self.ticks.is_multiple_of(5)) {
            self.show_cleanup();
        }
    }

    // ----- Clean junk -----------------------------------------------------

    fn junk_open(&mut self, open: bool) {
        if let Some(ui) = self.ui() {
            ui.set_junk_open(open);
        }
        if open {
            self.junk_refresh();
        }
    }

    /// Looks in the known places on a thread. `quiet` is the reminder's
    /// look: the home screen's card changes, the screen doesn't open.
    fn junk_find(&mut self, quiet: bool) {
        if self.junk_find_job.is_some() {
            return;
        }
        // A phone's junk is what the apps report found: measured again.
        if phone_junk() {
            self.junk_after_apps = true;
            self.start_apps_job();
            self.junk_refresh();
            return;
        }
        let result: Slot<Vec<freesize_core::junk::Finding>> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-junk".into())
            .spawn(move || {
                use freesize_core::junk::{Env, Platform, find_all, now, running_programs};
                let record = growth::data_dir().map(|dir| dir.join("programs.json"));
                let findings = find_all(
                    Platform::current(),
                    &Env::system(),
                    &running_programs(),
                    now(),
                    record.as_deref(),
                );
                put(&worker_result, findings);
            });
        match spawned {
            Ok(_) => {
                self.junk_find_job = Some(result);
                self.junk_quiet = quiet;
                self.ensure_ticking();
                self.junk_refresh();
            }
            Err(err) => self.set_status(&format!("Couldn't look for junk: {err}")),
        }
    }

    /// Runs the reminder's look when one is due.
    fn junk_remind(&mut self) {
        if !cfg!(target_os = "android") && self.junk.settings.due(apps::now() as i64) {
            self.junk_find(true);
        }
    }

    fn junk_found(&mut self, findings: Vec<freesize_core::junk::Finding>) {
        let now = apps::now() as i64;
        let outcome = self.junk.outcome.take();
        self.junk.found(findings, now);
        self.junk.outcome = outcome;
        if self.hub.state().is_some() {
            self.hub.report(freesize_proto::Report {
                junk: Some(freesize_agent::junk::report(&self.junk.findings, now)),
                ..freesize_proto::Report::default()
            });
        }
        if self.junk_quiet {
            self.junk_quiet = false;
            if self.junk.settings.found > 0 {
                self.set_status(&format!(
                    "Found {} of junk; {} is safe to clean. Review it on the home screen.",
                    format_size(self.junk.settings.found),
                    format_size(self.junk.settings.safe)
                ));
            }
        }
        self.junk_refresh();
        self.refresh_home();
    }

    fn junk_refresh(&self) {
        let Some(ui) = self.ui() else {
            return;
        };
        let looking = self.junk_find_job.is_some() || self.junk_after_apps;
        let cleaning = self.junk_clean_job.is_some();
        self.junk_rows.set_vec(self.junk.rows(looking && !cleaning));
        ui.set_junk_stage(self.junk.stage.key());
        ui.set_junk_headline(self.junk.headline().into());
        ui.set_junk_summary(self.junk.summary().into());
        ui.set_junk_busy(looking || cleaning);
        let chosen = self.junk.chosen_bytes();
        ui.set_junk_can_clean(chosen > 0 && !looking && !cleaning);
        ui.set_junk_clean_label(if chosen > 0 {
            format!("Clean {}", format_size(chosen)).into()
        } else {
            "Clean".into()
        });
        match self.junk.stage {
            junk::Stage::Cleaning => {
                let (steps, fraction, line) = self.junk.progress_view();
                ui.set_junk_steps(ModelRc::new(VecModel::from(steps)));
                ui.set_junk_fraction(fraction);
                ui.set_junk_progress_line(line.into());
                ui.set_junk_stopping(
                    self.junk
                        .progress
                        .as_ref()
                        .is_some_and(|p| p.stop.load(Ordering::Relaxed)),
                );
            }
            junk::Stage::Done => {
                let (title, text, steps) = self.junk.results_view();
                ui.set_junk_result_title(title.into());
                ui.set_junk_result_text(text.into());
                ui.set_junk_steps(ModelRc::new(VecModel::from(steps)));
                ui.set_junk_has_quarantined(
                    self.junk
                        .outcome
                        .as_ref()
                        .is_some_and(|o| o.quarantined > 0),
                );
            }
            junk::Stage::List => {}
        }
    }

    /// The phone's Clean junk screen, from what the apps report found.
    fn junk_from_report(&mut self) {
        let findings = self
            .report()
            .map(|r| junk::phone_findings(&r.junk))
            .unwrap_or_default();
        self.junk.phone = true;
        self.junk.found(findings, apps::now() as i64);
        self.junk_refresh();
    }

    /// Stop after the rule cleaning now.
    fn junk_stop(&mut self) {
        if let Some(progress) = &self.junk.progress {
            progress.stop.store(true, Ordering::Relaxed);
        }
        self.junk_refresh();
    }

    /// Back from the results to the list, looked at again.
    fn junk_done(&mut self) {
        self.junk.stage = junk::Stage::List;
        self.junk.outcome = None;
        self.junk_refresh();
    }

    /// Shows or hides a tool rule's command.
    fn junk_command(&mut self, index: i32) {
        if let Ok(index) = usize::try_from(index) {
            self.junk.toggle_command(index);
            self.junk_refresh();
        }
    }

    /// Run: asks first. Without the rights it needs, Windows opens
    /// FreeSize again as administrator on this screen instead.
    fn request_junk_run(&mut self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(command) = self.junk.command_of(index) else {
            return;
        };
        if self.command_job.is_some() {
            return;
        }
        if command.admin && !self.junk.elevated && cfg!(windows) {
            match elevate::relaunch_for_junk() {
                Ok(()) => {
                    self.junk_open(false);
                    self.leave_for_admin_window();
                }
                Err(err) => self.set_status(&format!("Couldn't open as administrator: {err}.")),
            }
            return;
        }
        let (Some(ui), Some(text)) = (self.ui(), self.junk.run_confirm_text(index)) else {
            return;
        };
        ui.set_confirm_text(text.into());
        ui.set_delete_label("Run".into());
        ui.set_confirm_open(true);
        self.pending_command = Some(index);
    }

    fn run_junk_command(&mut self, index: usize) {
        let (Some(command), Some(id)) = (self.junk.command_of(index), self.junk.rule_id(index))
        else {
            return;
        };
        let elevated = self.junk.elevated;
        let result: Slot<Result<String, String>> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-junk-command".into())
            .spawn(move || put(&worker_result, junk::run_command(command, elevated)));
        match spawned {
            Ok(_) => {
                self.command_job = Some((id, result));
                self.junk.command_running = Some(id);
                self.junk.command_output.remove(id);
                self.ensure_ticking();
                self.junk_refresh();
            }
            Err(err) => self.set_status(&format!("Couldn't run it: {err}")),
        }
    }

    fn junk_toggle(&mut self, index: i32) {
        if let Ok(index) = usize::try_from(index) {
            self.junk.toggle(index);
            self.junk_refresh();
        }
    }

    fn junk_expand(&mut self, index: i32) {
        if let Ok(index) = usize::try_from(index) {
            self.junk.toggle_open(index);
            self.junk_refresh();
        }
    }

    /// Shows an item's path in the file manager.
    fn junk_show(&mut self, id: i32) {
        if let Some(path) = self.junk.item_path(id)
            && let Err(err) = actions::reveal(&path)
        {
            self.set_status(&format!("Couldn't show {}: {err}", path.display()));
        }
    }

    /// Opens the settings page that removes a rule's junk.
    fn junk_tool(&mut self, index: i32) {
        let Some(finding) = usize::try_from(index)
            .ok()
            .and_then(|i| self.junk.findings.get(i))
        else {
            return;
        };
        let tool = finding.rule.tool;
        if let Some(url) = junk::tool_url(tool)
            && let Err(err) = actions::open_url(url)
        {
            self.set_status(&format!("Couldn't open {tool}: {err}"));
        }
    }

    /// Asks before cleaning what's ticked, with the shared confirmation
    /// dialog.
    fn request_junk_clean(&mut self) {
        if self.junk_clean_job.is_some() || self.junk_find_job.is_some() {
            return;
        }
        let text = self.junk.confirm_text();
        let Some(ui) = self.ui() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        ui.set_confirm_text(text.into());
        ui.set_delete_label("Clean".into());
        ui.set_confirm_open(true);
        self.pending_junk = true;
    }

    /// Cleans the ticked findings on a thread.
    fn run_junk_clean(&mut self) {
        let chosen: Vec<freesize_core::junk::Finding> =
            self.junk.chosen().into_iter().cloned().collect();
        if chosen.is_empty() {
            return;
        }
        let result: Slot<junk::Outcome> = Arc::default();
        let worker_result = result.clone();
        let progress: Arc<junk::Progress> = Arc::default();
        let worker_progress = progress.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-junk-clean".into())
            .spawn(move || put(&worker_result, junk::clean(chosen, &worker_progress)));
        match spawned {
            Ok(_) => {
                self.junk_clean_job = Some(result);
                self.junk.progress = Some(progress);
                self.junk.stage = junk::Stage::Cleaning;
                self.ensure_ticking();
                self.set_status("Cleaning…");
                self.junk_refresh();
            }
            Err(err) => self.set_status(&format!("Couldn't clean: {err}")),
        }
    }

    /// The quarantine changed (a delete, Put back or Empty, here or at the
    /// Hub's request): the lists, the home screen and the Hub hear of it.
    fn quarantine_changed(&mut self) {
        self.quarantine = quarantine::list();
        self.refresh_quarantine_view();
        if self.hub.state().is_some() {
            self.hub.report(freesize_proto::Report {
                quarantine: Some(self.quarantine.clone()),
                ..freesize_proto::Report::default()
            });
        }
        self.show_cleanup();
        self.refresh_home();
    }

    /// Asks before emptying `ids` (all batches when none are named), listing
    /// the items that would go for good.
    fn confirm_empty(&mut self, ids: Option<Vec<String>>) {
        let batches: Vec<_> = self
            .quarantine
            .iter()
            .filter(|b| ids.as_ref().is_none_or(|ids| ids.contains(&b.id)))
            .cloned()
            .collect();
        if batches.is_empty() || self.quarantine_job.is_some() {
            return;
        }
        let Some(ui) = self.ui() else {
            return;
        };
        ui.set_confirm_text(quarantine::confirm_text(&batches).into());
        ui.set_delete_label("Delete for good".into());
        ui.set_confirm_open(true);
        self.pending_empty = Some(batches.into_iter().map(|b| b.id).collect());
    }

    /// Shows the open batch's page as the quarantine is now, or closes it
    /// when the batch is gone.
    fn refresh_quarantine_view(&mut self) {
        let Some(ui) = self.ui() else {
            return;
        };
        let now = apps::now() as i64;
        let built = self
            .quarantine_view
            .as_mut()
            .and_then(|view| view.rows(&self.quarantine, now));
        match built {
            Some((head, rows)) => {
                ui.set_qv_title(head.title.into());
                ui.set_qv_subtitle(head.subtitle.into());
                ui.set_qv_crumbs(head.crumbs.into());
                ui.set_qv_picked(head.picked.into());
                ui.set_qv_rows(ModelRc::new(VecModel::from(rows)));
                ui.set_qv_busy(self.quarantine_job.is_some());
                ui.set_qv_open(true);
            }
            None => {
                self.quarantine_view = None;
                ui.set_qv_open(false);
            }
        }
    }

    /// Up a folder, or back to the quarantine.
    fn qv_back(&mut self) {
        if let Some(view) = &mut self.quarantine_view
            && view.folders.pop().is_none()
        {
            self.quarantine_view = None;
        }
        if self.quarantine_view.is_none()
            && let Some(ui) = self.ui()
        {
            ui.set_qv_open(false);
        }
        self.refresh_quarantine_view();
    }

    fn qv_toggle(&mut self, id: i32) {
        if let Some(view) = &mut self.quarantine_view {
            view.toggle(id);
        }
        self.refresh_quarantine_view();
    }

    /// A folder opens here; a file opens with its app, unless it's a
    /// program, which would run.
    fn qv_open_item(&mut self, id: i32) {
        let Some((path, _)) = self
            .quarantine_view
            .as_ref()
            .and_then(|v| v.target(id))
            .cloned()
        else {
            return;
        };
        if path.is_dir() {
            if let Some(view) = &mut self.quarantine_view {
                view.folders.push(path);
            }
            self.refresh_quarantine_view();
        } else if quarantine::runs_when_opened(&path) {
            self.set_status("Programs aren't opened from the quarantine; use Show in folder.");
        } else if let Err(err) = actions::open(&path) {
            self.set_status(&format!("Couldn't open it: {err}"));
        }
    }

    fn qv_reveal(&mut self, id: i32) {
        if let Some((path, _)) = self.quarantine_view.as_ref().and_then(|v| v.target(id))
            && let Err(err) = actions::reveal(path)
        {
            self.set_status(&format!("Couldn't show it: {err}"));
        }
    }

    /// The items a button acts on: one row's, or the ticked ones (all
    /// when none is ticked).
    fn qv_items(&self, id: Option<i32>) -> Option<(String, Vec<String>)> {
        let view = self.quarantine_view.as_ref()?;
        let batch = self.quarantine.iter().find(|b| b.id == view.batch)?;
        let items = match id {
            Some(id) => vec![view.target(id)?.1.clone()?],
            None => view.chosen(batch),
        };
        Some((view.batch.clone(), items))
    }

    /// Puts items back; when something new is at an item's old place,
    /// asks what to do first.
    fn qv_restore(&mut self, id: Option<i32>) {
        let Some((batch, items)) = self.qv_items(id) else {
            return;
        };
        if self.quarantine_job.is_some() {
            return;
        }
        let taken = quarantine::taken(&batch, &items);
        if taken.is_empty() {
            self.restore_items(batch, items, quarantine::Conflict::Skip);
            return;
        }
        let Some(ui) = self.ui() else {
            return;
        };
        let mut text = if taken.len() == 1 {
            format!(
                "Something new is at {} now. Keep both (the one put back is named \
                 “… (put back)”), replace it (it goes to the quarantine instead), or skip it?",
                taken[0]
            )
        } else {
            format!(
                "Something new is where {} of these were. Keep both (those put back are named \
                 “… (put back)”), replace them (what's there goes to the quarantine \
                 instead), or skip them?",
                taken.len()
            )
        };
        if taken.len() > 1 {
            for name in taken.iter().take(8) {
                text.push_str("\n• ");
                text.push_str(name);
            }
        }
        ui.set_conflict_text(text.into());
        ui.set_conflict_open(true);
        self.pending_restore = Some((batch, items));
    }

    fn conflict_chosen(&mut self, choice: i32) {
        if let Some(ui) = self.ui() {
            ui.set_conflict_open(false);
        }
        let Some((batch, items)) = self.pending_restore.take() else {
            return;
        };
        let conflict = match choice {
            0 => quarantine::Conflict::KeepBoth,
            1 => quarantine::Conflict::Replace,
            2 => quarantine::Conflict::Skip,
            _ => return,
        };
        self.restore_items(batch, items, conflict);
    }

    fn restore_items(&mut self, batch: String, items: Vec<String>, conflict: quarantine::Conflict) {
        self.run_quarantine_job("Putting back…", move || {
            quarantine::restore_items(&batch, &items, conflict)
                .map(|text| capitalize_first(&text) + ".")
        });
        self.refresh_quarantine_view();
    }

    /// Deletes items for good, after the confirmation listing them.
    fn qv_delete(&mut self, id: Option<i32>) {
        let Some((batch, items)) = self.qv_items(id) else {
            return;
        };
        let Some(whole) = self.quarantine.iter().find(|b| b.id == batch).cloned() else {
            return;
        };
        if self.quarantine_job.is_some() {
            return;
        }
        let chosen = freesize_proto::unraid::QuarantineBatch {
            items: whole
                .items
                .iter()
                .filter(|i| items.contains(&i.stored))
                .cloned()
                .collect(),
            ..whole
        };
        let chosen = freesize_proto::unraid::QuarantineBatch {
            bytes: chosen.items.iter().map(|i| i.bytes).sum(),
            ..chosen
        };
        let Some(ui) = self.ui() else {
            return;
        };
        ui.set_confirm_text(quarantine::confirm_text(std::slice::from_ref(&chosen)).into());
        ui.set_delete_label("Delete for good".into());
        ui.set_confirm_open(true);
        self.pending_empty_items = Some((batch, items));
    }

    fn restore_batch(&mut self, id: String) {
        self.run_quarantine_job("Putting back…", move || {
            quarantine::restore(&id).map(|text| capitalize_first(&text) + ".")
        });
    }

    fn empty_batches(&mut self, ids: Vec<String>) {
        self.run_quarantine_job("Emptying the quarantine…", move || {
            quarantine::empty(&ids)
                .map(|bytes| format!("Emptied the quarantine, freeing {}.", format_size(bytes)))
        });
    }

    fn run_quarantine_job(
        &mut self,
        status: &str,
        job: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        if self.quarantine_job.is_some() {
            return;
        }
        let result: Slot<Result<String, String>> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-quarantine".into())
            .spawn(move || put(&worker_result, job()));
        match spawned {
            Ok(_) => {
                self.quarantine_job = Some(result);
                self.ensure_ticking();
                self.set_status(status);
            }
            Err(err) => self.set_status(&format!("Couldn't start that: {err}")),
        }
    }

    /// The unreadable-folders button: a scan with more rights, or the
    /// setting that grants them, as `elevate::offer` worked out.
    fn fuller_scan(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        if self.exporting.is_some() || self.scan.is_some() {
            return;
        }
        let root = tree.path(tree.root());
        let offer = elevate::offer(freesize_core::access::rights(), &root, self.imported);
        if offer.action.is_empty() {
            return;
        }
        match elevate::start(&offer.action, &root) {
            Ok(elevate::Started::NewWindow) => self.leave_for_admin_window(),
            Ok(elevate::Started::Exporting(exporting)) => {
                self.set_status(&format!(
                    "Scanning {} with more rights in the background; it opens here when done.",
                    root.display()
                ));
                self.exporting = Some(exporting);
                self.ensure_ticking();
            }
            Ok(elevate::Started::Opened) => {
                self.set_status("Turn on Full Disk Access for FreeSize there, then scan again.");
            }
            Err(reason) => self.set_status(&format!("Couldn't scan with more rights: {reason}.")),
        }
    }

    /// The Similar photos button: starts the search, or stops it while it
    /// runs.
    fn photo_action(&mut self) {
        if matches!(self.photos, PhotoSearch::Running { .. }) {
            self.stop_photos();
            self.show_cleanup();
            return;
        }
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let photos = similar::candidates(tree, tree.root());
        if photos.is_empty() {
            self.photos = PhotoSearch::Done {
                groups: Vec::new(),
                previews: HashMap::new(),
            };
            self.show_cleanup();
            return;
        }
        let progress = Arc::new(similar::Progress::new());
        let result: Slot<Vec<similar::Group>> = Arc::default();
        let spawned = {
            let (progress, result) = (progress.clone(), result.clone());
            std::thread::Builder::new()
                .name("freesize-photos".into())
                .spawn(move || put(&result, similar::find(&photos, &progress)))
        };
        if let Err(err) = spawned {
            ui.set_status(format!("Could not start looking at photos: {err}").into());
            return;
        }
        self.photos = PhotoSearch::Running { progress, result };
        self.show_cleanup();
        self.ensure_ticking();
    }

    fn stop_photos(&mut self) {
        if let PhotoSearch::Running { progress, .. } = &self.photos {
            progress.cancel();
        }
        self.photos = PhotoSearch::Idle;
    }

    /// The Duplicate files button: starts the search, or stops it while it
    /// runs.
    fn duplicate_action(&mut self) {
        if matches!(self.dupes, DupeSearch::Running { .. }) {
            self.stop_duplicates();
            self.stop_photos();
            self.show_cleanup();
            return;
        }
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let candidates = duplicate_candidates(tree, tree.root(), filecleanup::MIN_DUPLICATE);
        if candidates.files() == 0 {
            self.dupes = DupeSearch::Done(Vec::new());
            self.show_cleanup();
            return;
        }
        let progress = Arc::new(DuplicateProgress::new());
        let result: Slot<Vec<DuplicateGroup>> = Arc::default();
        let spawned = {
            let (progress, result) = (progress.clone(), result.clone());
            std::thread::Builder::new()
                .name("freesize-duplicates".into())
                .spawn(move || put(&result, candidates.find(&progress)))
        };
        if let Err(err) = spawned {
            ui.set_status(format!("Could not start the duplicate search: {err}").into());
            return;
        }
        self.dupes = DupeSearch::Running { progress, result };
        self.show_cleanup();
        self.ensure_ticking();
    }

    fn stop_duplicates(&mut self) {
        if let DupeSearch::Running { progress, .. } = &self.dupes {
            progress.cancel();
        }
        self.dupes = DupeSearch::Idle;
    }

    // ----- Treemap and lists ----------------------------------------------

    fn map_resized(&mut self, width: f32, height: f32) {
        if (width, height) != self.map_size {
            self.map_size = (width, height);
            self.update_treemap();
        }
    }

    fn bubbles_resized(&mut self, width: f32, height: f32) {
        if (width, height) != self.bubble_size {
            self.bubble_size = (width, height);
            self.update_bubbles();
        }
    }

    fn refresh_focus_views(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let focus = tree.node(self.focus);
        ui.set_crumbs(ModelRc::new(VecModel::from(self.crumbs(tree))));
        ui.set_focus_size(format_size(focus.size_in(self.mode)).into());
        self.update_map();
        self.update_lists();
    }

    /// Draws whichever of the treemap and the bubbles is showing.
    fn update_map(&self) {
        match self.ui() {
            Some(ui) if ui.get_bubbles_view() => self.update_bubbles(),
            Some(_) => self.update_treemap(),
            None => {}
        }
    }

    /// The path from the scanned folder to the shown one. Deep paths keep the
    /// first step and the last two, with "…" standing for the ones between.
    fn crumbs(&self, tree: &Tree) -> Vec<Crumb> {
        let mut chain: Vec<NodeId> = tree.ancestors(self.focus).collect();
        chain.reverse();
        let root = tree.root();
        let root_label = self.root_label(tree);
        let crumb = |id: NodeId, label: Option<&str>| Crumb {
            id: id as i32,
            label: match label {
                Some(label) => label.into(),
                None if id == root => root_label.as_str().into(),
                None => tree.name(id).as_ref().into(),
            },
        };
        if chain.len() <= 4 {
            return chain.iter().map(|&id| crumb(id, None)).collect();
        }
        let n = chain.len();
        vec![
            crumb(chain[0], None),
            crumb(chain[n - 3], Some("…")),
            crumb(chain[n - 2], None),
            crumb(chain[n - 1], None),
        ]
    }

    /// The icon for a node: the app or game it belongs to, "can be
    /// rebuilt", or the kind of content.
    fn icon_of(&self, tree: &Tree, id: NodeId) -> (i32, Option<Image>) {
        let app = tree
            .node(id)
            .is_dir()
            .then(|| self.owners.icon(&tree.path(id)))
            .flatten();
        if self.rebuildable.contains(&id) {
            return (ICON_REBUILD, app);
        }
        let kind = self.kinds.get(id as usize).copied().unwrap_or_default();
        (kind as i32, app)
    }

    fn update_bubbles(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let (width, height) = self.bubble_size;
        if width < 40.0 || height < 40.0 || !ui.get_bubbles_view() {
            return;
        }
        let items = bubbles::layout(
            tree,
            self.focus,
            self.mode,
            width,
            height,
            &BubbleOptions::default(),
        );
        let before = self.compare_sizes();
        let out: Vec<BubbleItem> = items
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let r = b.circle.r;
                let pill = b.depth == 0
                    && items
                        .get(i + 1)
                        .is_some_and(|next| next.depth == 1 && next.group == b.group);
                let (color, text_color) = match b.node {
                    Some(id) => block_color(b.group, b.depth * 2, tree.node(id).is_dir()),
                    None => (
                        Color::from_rgb_u8(0x9e, 0xa3, 0xab),
                        Color::from_rgb_u8(0x1b, 0x1b, 0x1f),
                    ),
                };
                let (label, detail, icon_size) = if b.depth == 0 {
                    let name = match b.node {
                        Some(id) => tree.name(id).into_owned(),
                        None => format!("{} more", format_count(u64::from(b.count))),
                    };
                    if r >= 26.0 {
                        let detail = if r >= 38.0 {
                            // With an earlier scan, also how much it grew.
                            let growth = b
                                .node
                                .filter(|&id| tree.node(id).is_dir())
                                .and_then(|id| {
                                    before.get(snapshot::relative_path(tree, id).as_str())
                                })
                                .map(|&was| b.size as i64 - was as i64)
                                .filter(|d| {
                                    d.unsigned_abs() >= (b.size / 20).max(10 * 1024 * 1024)
                                });
                            match growth {
                                Some(d) => format!(
                                    "{} · {}{}",
                                    format_size(b.size),
                                    if d > 0 { "+" } else { "−" },
                                    format_size(d.unsigned_abs())
                                ),
                                None => format_size(b.size),
                            }
                        } else {
                            String::new()
                        };
                        let icon = if r >= 46.0 { (r * 0.36).min(52.0) } else { 0.0 };
                        (name, detail, icon)
                    } else {
                        (
                            String::new(),
                            String::new(),
                            if r >= 12.0 { r } else { 0.0 },
                        )
                    }
                } else {
                    let icon = if r >= 12.0 { (r * 0.9).min(26.0) } else { 0.0 };
                    (String::new(), String::new(), icon)
                };
                let (icon, app_icon) = match b.node {
                    Some(id) if icon_size > 0.0 => self.icon_of(tree, id),
                    Some(_) => (0, None),
                    None => (ICON_MORE, None),
                };
                BubbleItem {
                    id: b.node.map_or(-1, |id| id as i32),
                    key: SharedString::new(),
                    x: b.circle.x,
                    y: b.circle.y,
                    r,
                    color,
                    text_color,
                    label: label.into(),
                    detail: detail.into(),
                    depth: i32::from(b.depth),
                    icon,
                    has_app_icon: app_icon.is_some(),
                    app_icon: app_icon.unwrap_or_default(),
                    icon_size,
                    ring: b.node.is_some_and(|id| self.rebuildable.contains(&id)),
                    pill,
                }
            })
            .collect();
        ui.set_bubbles(ModelRc::new(VecModel::from(out)));
    }

    fn update_treemap(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let (width, height) = self.map_size;
        if width < 20.0 || height < 20.0 {
            ui.set_map_rects(ModelRc::default());
            return;
        }
        if ui.get_bubbles_view() {
            return;
        }
        let items = treemap::layout(
            tree,
            self.focus,
            self.mode,
            Rect::new(0.0, 0.0, width, height),
            &TreemapOptions::default(),
        );
        let rects: Vec<MapRect> = items
            .iter()
            .map(|item| {
                let node = tree.node(item.node);
                let r = item.rect;
                let (color, text_color) = block_color(item.group, item.depth, node.is_dir());
                let name = tree.name(item.node);
                let size = format_size(node.size_in(self.mode));
                let label = if r.w < 34.0 || r.h < 14.0 {
                    String::new()
                } else if item.nested {
                    format!("{name}  {size}")
                } else if r.h >= 30.0 {
                    format!("{name}\n{size}")
                } else {
                    name.into_owned()
                };
                // Icons on the bigger blocks, and beside folder names.
                let icon_size = if item.nested {
                    if label.is_empty() { 0.0 } else { 12.0 }
                } else if r.w >= 56.0 && r.h >= 72.0 {
                    (r.w.min(r.h) * 0.4).min(48.0)
                } else {
                    0.0
                };
                let (icon, app_icon) = if icon_size > 0.0 {
                    self.icon_of(tree, item.node)
                } else {
                    (0, None)
                };
                MapRect {
                    id: item.node as i32,
                    x: r.x,
                    y: r.y,
                    width: r.w,
                    height: r.h,
                    color,
                    text_color,
                    label: label.into(),
                    nested: item.nested,
                    icon,
                    has_app_icon: app_icon.is_some(),
                    app_icon: app_icon.unwrap_or_default(),
                    icon_size,
                    ring: self.rebuildable.contains(&item.node),
                }
            })
            .collect();
        ui.set_map_rects(ModelRc::new(VecModel::from(rects)));
    }

    fn update_lists(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let folder_total = tree.node(self.focus).size_in(self.mode).max(1) as f64;
        let stats = extension_stats(tree, self.focus, self.mode);
        let now = apps::now() as i64;
        let ages = age_bins(tree, self.focus, self.mode, now);
        let sizes = size_bins(tree, self.focus, self.mode);

        // The group the files list is limited to: its name, file count and
        // bytes, and which files belong to it.
        let group: Option<(String, u64, u64)> = self.file_filter.as_ref().map(|filter| {
            let (name, found) = match filter {
                FileFilter::Type(ext) => (
                    if ext.is_empty() {
                        "Files without an extension".to_string()
                    } else {
                        format!("Only .{ext}")
                    },
                    stats
                        .iter()
                        .find(|s| &s.extension == ext)
                        .map(|s| (s.files, s.size)),
                ),
                FileFilter::Modified(bin) => (
                    match bin.label.as_str() {
                        "No date" => "Files without a date".to_string(),
                        label if label.starts_with("Before") => {
                            format!("Changed {}", label.to_lowercase())
                        }
                        label => format!("Changed in {label}"),
                    },
                    ages.iter()
                        .find(|b| b.from == bin.from)
                        .map(|b| (b.files, b.size)),
                ),
                FileFilter::Size(bin) => (
                    // "Sized under 1 MB", "Sized 1 GB and up".
                    format!("Sized {}{}", bin.label[..1].to_lowercase(), &bin.label[1..]),
                    sizes
                        .iter()
                        .find(|b| b.from == bin.from)
                        .map(|b| (b.files, b.size)),
                ),
                FileFilter::OlderThan(time) => (
                    format!(
                        "Not changed since {}",
                        insights::date((*time).max(0) as u64)
                    ),
                    Some(
                        tree.descendants(self.focus)
                            .filter(|&id| {
                                !tree.node(id).is_dir() && filter.matches(tree, id, self.mode)
                            })
                            .fold((0, 0), |(files, bytes), id| {
                                (files + 1, bytes + tree.node(id).size_in(self.mode))
                            }),
                    ),
                ),
            };
            let (files, bytes) = found.unwrap_or((0, 0));
            (name, files, bytes)
        });
        // Bars compare a listed file with its group, else with the folder.
        let total = group
            .as_ref()
            .map_or(folder_total, |&(_, _, bytes)| bytes.max(1) as f64);

        let found = match &self.file_filter {
            Some(filter) => largest_files_where(tree, self.focus, self.mode, MAX_OF_TYPE, |id| {
                filter.matches(tree, id, self.mode)
            }),
            None => largest_files(tree, self.focus, self.mode, 100),
        };
        ui.set_files_filter(match &group {
            Some((name, count, bytes)) => {
                let mut text = format!(
                    "{name} · {}, {}",
                    plural(*count, "file", "files"),
                    format_size(*bytes)
                );
                if *count > MAX_OF_TYPE as u64 {
                    text.push_str(&format!(
                        " · biggest {} shown",
                        format_count(MAX_OF_TYPE as u64)
                    ));
                }
                text.into()
            }
            None => SharedString::new(),
        });
        let files: Vec<FileRow> = found
            .into_iter()
            .map(|id| {
                let size = tree.node(id).size_in(self.mode);
                let parent = tree.node(id).parent;
                let folder = relative_path(tree, self.focus, parent);
                FileRow {
                    id: id as i32,
                    name: tree.name(id).as_ref().into(),
                    folder: if folder.is_empty() {
                        "(this folder)".into()
                    } else {
                        folder.into()
                    },
                    size: format_size(size).into(),
                    fraction: (size as f64 / total) as f32,
                }
            })
            .collect();
        ui.set_largest_files(ModelRc::new(VecModel::from(files)));

        // The Types tab shows types, ages or sizes; tapping a row lists its
        // files (see `pick_group`).
        let bin_row = |prefix: &str, i: usize, bin: &Bin, color: Color| TypeRow {
            key: format!("{prefix}{i}").into(),
            label: bin.label.as_str().into(),
            size: format_size(bin.size).into(),
            files: plural(bin.files, "file", "files").into(),
            fraction: (bin.size as f64 / folder_total) as f32,
            color,
        };
        let rows: Vec<TypeRow> = match ui.get_types_view() {
            1 => ages
                .iter()
                .enumerate()
                .map(|(i, bin)| {
                    // Newer is bluer, older greyer.
                    let fade = (i as f32 / ages.len().max(1) as f32).min(1.0);
                    bin_row(
                        AGE_KEY,
                        i,
                        bin,
                        hsl(210.0, 0.65 * (1.0 - fade) + 0.05, 0.55),
                    )
                })
                .collect(),
            2 => sizes
                .iter()
                .enumerate()
                .map(|(i, bin)| bin_row(SIZE_KEY, i, bin, hsl(25.0 + 30.0 * i as f32, 0.7, 0.55)))
                .collect(),
            _ => stats
                .iter()
                .take(60)
                .map(|stat| TypeRow {
                    key: format!("{TYPE_KEY}{}", stat.extension).into(),
                    label: if stat.extension.is_empty() {
                        "(no extension)".into()
                    } else {
                        format!(".{}", stat.extension).into()
                    },
                    size: format_size(stat.size).into(),
                    files: plural(stat.files, "file", "files").into(),
                    fraction: (stat.size as f64 / folder_total) as f32,
                    color: extension_color(&stat.extension),
                })
                .collect(),
        };
        ui.set_file_types(ModelRc::new(VecModel::from(rows)));
        *self.bins.borrow_mut() = (ages, sizes);
    }

    /// Lists the files of one row of the Types tab: a type, an age or a size.
    fn pick_group(&mut self, key: &str) {
        let (ages, sizes) = self.bins.borrow().clone();
        let index = |rest: &str| rest.parse::<usize>().ok();
        self.file_filter = if let Some(ext) = key.strip_prefix(TYPE_KEY) {
            Some(FileFilter::Type(ext.to_string()))
        } else if let Some(i) = key.strip_prefix(AGE_KEY).and_then(index) {
            ages.get(i).cloned().map(FileFilter::Modified)
        } else if let Some(i) = key.strip_prefix(SIZE_KEY).and_then(index) {
            sizes.get(i).cloned().map(FileFilter::Size)
        } else {
            None
        };
        self.update_lists();
        if let Some(ui) = self.ui() {
            // Show the files: the "Largest files" tab, or "Largest" on phones.
            ui.set_wide_tab(1);
            ui.set_compact_tab(2);
        }
    }

    fn clear_file_filter(&mut self) {
        self.file_filter = None;
        self.update_lists();
    }

    // ----- Basket -----------------------------------------------------------

    /// Adds the selected item to the basket, or takes it out again.
    fn toggle_collect(&mut self) {
        self.toggle_basket(self.selected);
    }

    /// Adds an item to the basket, or takes it out again.
    fn toggle_basket(&mut self, id: NodeId) {
        let Some(tree) = &self.tree else {
            return;
        };
        if id == tree.root() || self.imported {
            return;
        }
        if let Some(pos) = self.basket.iter().position(|&b| b == id) {
            self.basket.remove(pos);
        } else if let Some(&holder) = self
            .basket
            .iter()
            .find(|&&b| tree.ancestors(id).skip(1).any(|a| a == b))
        {
            let text = format!("Already selected with the folder {}.", tree.name(holder));
            self.set_status(&text);
            return;
        } else {
            // A folder takes the place of anything collected inside it.
            self.basket
                .retain(|&b| !tree.ancestors(b).skip(1).any(|a| a == id));
            self.basket.push(id);
        }
        self.refresh_basket();
    }

    /// Puts every file the filtered files list shows in the basket, to look
    /// over there and delete or move together.
    fn collect_listed(&mut self) {
        let (Some(tree), Some(filter)) = (&self.tree, &self.file_filter) else {
            return;
        };
        if self.imported {
            return;
        }
        let listed = largest_files_where(tree, self.focus, self.mode, MAX_OF_TYPE, |id| {
            filter.matches(tree, id, self.mode)
        });
        for id in listed {
            let covered = tree.ancestors(id).any(|a| self.basket.contains(&a));
            if !covered {
                self.basket.push(id);
            }
        }
        // The selection bar says what's selected; no second message here.
        self.refresh_basket();
    }

    /// Puts several items in the basket (the Clean up tab's "select all"),
    /// skipping what's in a folder already there.
    fn add_to_basket(&mut self, ids: &[NodeId]) {
        let Some(tree) = &self.tree else {
            return;
        };
        if self.imported {
            return;
        }
        for &id in ids {
            if id == tree.root() || tree.ancestors(id).any(|a| self.basket.contains(&a)) {
                continue;
            }
            // A folder takes the place of anything selected inside it.
            self.basket
                .retain(|&b| !tree.ancestors(b).skip(1).any(|a| a == id));
            self.basket.push(id);
        }
        // The page and the selection bar say what's selected.
        self.refresh_basket();
    }

    fn remove_from_basket(&mut self, id: i32) {
        self.basket.retain(|&b| b as i32 != id);
        self.refresh_basket();
    }

    fn clear_basket(&mut self) {
        self.basket.clear();
        self.refresh_basket();
    }

    fn open_basket(&mut self, open: bool) {
        self.move_targets.clear();
        if let Some(ui) = self.ui() {
            ui.set_basket_open(open);
        }
        self.refresh_basket();
    }

    fn refresh_basket(&self) {
        let Some(ui) = self.ui() else {
            return;
        };
        let rows: Vec<BasketRow> = match &self.tree {
            Some(tree) => self
                .basket
                .iter()
                .map(|&id| BasketRow {
                    id: id as i32,
                    name: tree.name(id).as_ref().into(),
                    place: {
                        let place = relative_path(tree, tree.root(), tree.node(id).parent);
                        if place.is_empty() {
                            self.root_label(tree)
                        } else {
                            place
                        }
                    }
                    .into(),
                    size: format_size(tree.node(id).size_in(self.mode)).into(),
                })
                .collect(),
            None => Vec::new(),
        };
        let bytes: u64 = self.tree.as_ref().map_or(0, |tree| {
            self.basket
                .iter()
                .map(|&id| tree.node(id).size_in(self.mode))
                .sum()
        });
        ui.set_basket_count(rows.len() as i32);
        ui.set_basket_summary(
            format!(
                "{}, {}",
                plural(rows.len() as u64, "item", "items"),
                format_size(bytes)
            )
            .into(),
        );
        ui.set_basket_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_in_basket(self.basket.contains(&self.selected));
        ui.set_put_back_label(
            self.last_move
                .as_ref()
                .map(|m| {
                    format!(
                        "{} to {}",
                        plural(m.moved.len() as u64, "item", "items"),
                        m.folder.display()
                    )
                })
                .unwrap_or_default()
                .into(),
        );
        let targets: Vec<SharedString> = self
            .move_targets
            .iter()
            .map(|p| p.to_string_lossy().as_ref().into())
            .collect();
        ui.set_move_targets(ModelRc::new(VecModel::from(targets)));
        self.mark_cleanup();
    }

    fn request_basket_delete(&mut self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        if self.basket.is_empty() || self.basket_job.is_some() {
            return;
        }
        let bytes: u64 = self
            .basket
            .iter()
            .map(|&id| tree.node(id).size_in(self.mode))
            .sum();
        let what = format!(
            "the {} selected ({})",
            plural(self.basket.len() as u64, "item", "items"),
            format_size(bytes)
        );
        let mut text = format!(
            "Move {what} to the quarantine? Put back returns it; the space is freed when you \
             empty the quarantine."
        );
        // Sets of copies where none would be left.
        if let DupeSearch::Done(groups) = &self.dupes {
            let covered = |id: NodeId| tree.ancestors(id).any(|a| self.basket.contains(&a));
            let whole = groups
                .iter()
                .filter(|g| {
                    let left: Vec<NodeId> = g
                        .nodes
                        .iter()
                        .copied()
                        .filter(|&id| {
                            !tree
                                .ancestors(id)
                                .any(|a| tree.node(a).has(freesize_core::flags::DELETED))
                        })
                        .collect();
                    left.len() > 1 && left.iter().all(|&id| covered(id))
                })
                .count();
            if whole > 0 {
                text.push_str(&format!(
                    " Note: every copy of {} is selected, so no copy of {} would be left.",
                    plural(whole as u64, "duplicate file", "duplicate files"),
                    if whole == 1 { "it" } else { "them" }
                ));
            }
        }
        self.pending_basket = true;
        ui.set_basket_open(false);
        ui.set_confirm_text(text.into());
        ui.set_confirm_open(true);
    }

    /// On phones: where the basket can be moved, a "FreeSize moved" folder
    /// on each drive.
    fn show_move_targets(&mut self) {
        self.move_targets = locations::list()
            .into_iter()
            .map(|l| l.path.join("FreeSize moved"))
            .collect();
        self.refresh_basket();
    }

    fn move_to_target(&mut self, index: i32) {
        let target = usize::try_from(index)
            .ok()
            .and_then(|i| self.move_targets.get(i).cloned());
        self.move_targets.clear();
        if let Some(target) = target {
            self.run_basket(BasketAction::Move(target));
        }
    }

    /// Deletes or moves everything in the basket on a worker thread.
    fn run_basket(&mut self, action: BasketAction) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        if self.basket.is_empty() || self.basket_job.is_some() {
            return;
        }
        let root_path = tree.path(tree.root());
        let items: Vec<(NodeId, PathBuf, u64)> = self
            .basket
            .iter()
            .map(|&id| (id, tree.path(id), tree.node(id).size_in(self.mode)))
            .collect();
        let verb = match &action {
            BasketAction::Delete => "Deleting",
            BasketAction::Move(_) => "Moving",
        };
        let result: Slot<BasketOutcome> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-basket".into())
            .spawn(move || {
                let mut outcome = BasketOutcome {
                    done: Vec::new(),
                    failed: Vec::new(),
                    bytes: 0,
                    moved: None,
                };
                let mut moved = Vec::new();
                // Everything deleted at once is one batch in the quarantine.
                let mut deleted = match &action {
                    BasketAction::Delete => {
                        let paths: Vec<PathBuf> = items.iter().map(|(_, p, _)| p.clone()).collect();
                        actions::delete_each(&paths).into_iter()
                    }
                    BasketAction::Move(_) => Vec::new().into_iter(),
                };
                for (id, path, size) in items {
                    let result = match &action {
                        BasketAction::Delete => {
                            deleted.next().unwrap_or_else(|| Err("not moved".into()))
                        }
                        BasketAction::Move(destination) => {
                            let to = moves::target(&root_path, &path, destination);
                            let result = moves::move_item(&path, &to).map_err(|e| e.to_string());
                            if result.is_ok() {
                                moved.push((path.clone(), to));
                            }
                            result
                        }
                    };
                    match result {
                        Ok(()) => {
                            outcome.done.push(id);
                            outcome.bytes += size;
                        }
                        Err(err) => outcome.failed.push(format!("{}: {err}", path.display())),
                    }
                }
                if let BasketAction::Move(destination) = &action
                    && !moved.is_empty()
                {
                    let manifest = moves::Manifest {
                        when: apps::now(),
                        folder: moves::target(&root_path, &root_path, destination),
                        moved,
                    };
                    moves::save(&manifest);
                    outcome.moved = Some(manifest);
                }
                put(&worker_result, outcome);
            });
        if let Err(err) = spawned {
            ui.set_status(format!("Could not start: {err}").into());
            return;
        }
        ui.set_basket_open(false);
        ui.set_status(
            format!(
                "{verb} {}…",
                plural(self.basket.len() as u64, "item", "items")
            )
            .into(),
        );
        self.basket_job = Some(result);
        self.ensure_ticking();
    }

    fn finish_basket(&mut self, outcome: BasketOutcome) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        for &id in &outcome.done {
            tree.mark_deleted(id);
        }
        self.basket.retain(|b| !outcome.done.contains(b));
        let expanded = self.rows.expanded_nodes();
        self.rows.rebuild(tree, self.mode, &expanded);
        let count = plural(outcome.done.len() as u64, "item", "items");
        let mut text = match &outcome.moved {
            Some(manifest) => format!(
                "Moved {count} ({}) to {}.",
                format_size(outcome.bytes),
                manifest.folder.display()
            ),
            None => format!(
                "Moved {count} ({}) to the quarantine; empty it to free the space.",
                format_size(outcome.bytes)
            ),
        };
        if let Some(err) = outcome.failed.first() {
            text.push_str(&format!(
                " {} failed, for example {err}",
                outcome.failed.len()
            ));
        }
        if outcome.moved.is_some() {
            self.last_move = outcome.moved;
        }
        // Show the folder again from the top of what's left.
        let focus = self.focus;
        self.focus = NO_NODE;
        let still_there = self.tree.as_ref().is_some_and(|t| {
            !t.ancestors(focus)
                .any(|a| t.node(a).has(freesize_core::flags::DELETED))
        });
        let tree_root = self.tree.as_ref().map_or(0, |t| t.root());
        self.select(if still_there { focus } else { tree_root } as i32, false);
        self.refresh_cleanup();
        self.refresh_basket();
        self.set_status(&text);
    }

    fn put_back(&mut self) {
        let Some(manifest) = self.last_move.clone() else {
            return;
        };
        if self.put_back_job.is_some() {
            return;
        }
        let result: Slot<(usize, Vec<String>)> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-put-back".into())
            .spawn(move || put(&worker_result, moves::put_back(&manifest)));
        if spawned.is_ok() {
            if let Some(ui) = self.ui() {
                ui.set_basket_open(false);
            }
            self.set_status("Putting things back…");
            self.put_back_job = Some(result);
            self.ensure_ticking();
        }
    }

    // ----- Actions --------------------------------------------------------

    fn reveal_selected(&mut self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        if let Err(err) = actions::reveal(&tree.path(self.selected)) {
            ui.set_status(format!("Could not open the file manager: {err}").into());
        }
    }

    fn open_selected(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        if self.selected == tree.root() && cfg!(target_os = "android") {
            return;
        }
        let path = tree.path(self.selected);
        let name = tree.name(self.selected).into_owned();
        match actions::open(&path) {
            Ok(actions::Open::Done) => {}
            Ok(actions::Open::TryAgain) => {
                self.set_status(&format!("Getting {name} ready to open…"));
                self.pending_open = Some((path, 0));
                self.ensure_ticking();
            }
            Err(err) => self.set_status(&format!("Could not open {name}: {err}")),
        }
    }

    fn request_delete(&mut self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        if self.selected == tree.root() || self.deletion.is_some() {
            return;
        }
        let node = tree.node(self.selected);
        let name = tree.name(self.selected);
        let size = format_size(node.size_in(self.mode));
        let what = if node.is_dir() {
            format!(
                "the folder “{name}” ({size}, {})",
                plural(u64::from(node.files), "file", "files")
            )
        } else {
            format!("“{name}” ({size})")
        };
        let text = format!(
            "Move {what} to the quarantine? Put back returns it; the space is freed when you \
             empty the quarantine."
        );
        ui.set_confirm_text(text.into());
        ui.set_confirm_open(true);
    }

    fn cancel_delete(&mut self) {
        self.pending_cleanup = None;
        self.pending_basket = false;
        self.pending_empty = None;
        self.pending_junk = false;
        self.pending_command = None;
        self.pending_empty_items = None;
        if let Some(ui) = self.ui() {
            ui.set_confirm_open(false);
            ui.set_delete_label(actions::DELETE_LABEL.into());
        }
    }

    fn confirm_delete(&mut self) {
        if let Some(ids) = self.pending_empty.take() {
            if let Some(ui) = self.ui() {
                ui.set_confirm_open(false);
                ui.set_delete_label(actions::DELETE_LABEL.into());
            }
            self.empty_batches(ids);
            return;
        }
        if std::mem::take(&mut self.pending_basket) {
            if let Some(ui) = self.ui() {
                ui.set_confirm_open(false);
                ui.set_delete_label(actions::DELETE_LABEL.into());
            }
            self.run_basket(BasketAction::Delete);
            return;
        }
        if let Some(kind) = self.pending_cleanup.take() {
            if let Some(ui) = self.ui() {
                ui.set_confirm_open(false);
                ui.set_delete_label(actions::DELETE_LABEL.into());
            }
            self.run_cleanup(kind);
            return;
        }
        if std::mem::take(&mut self.pending_junk) {
            if let Some(ui) = self.ui() {
                ui.set_confirm_open(false);
                ui.set_delete_label(actions::DELETE_LABEL.into());
            }
            self.run_junk_clean();
            return;
        }
        if let Some((batch, items)) = self.pending_empty_items.take() {
            if let Some(ui) = self.ui() {
                ui.set_confirm_open(false);
                ui.set_delete_label(actions::DELETE_LABEL.into());
            }
            self.run_quarantine_job("Deleting for good…", move || {
                quarantine::empty_items(&batch, &items)
                    .map(|bytes| format!("Deleted for good, freeing {}.", format_size(bytes)))
            });
            return;
        }
        if let Some(index) = self.pending_command.take() {
            if let Some(ui) = self.ui() {
                ui.set_confirm_open(false);
                ui.set_delete_label(actions::DELETE_LABEL.into());
            }
            self.run_junk_command(index);
            return;
        }
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        ui.set_confirm_open(false);
        if self.selected == tree.root() || self.deletion.is_some() {
            return;
        }
        let path = tree.path(self.selected);
        let name = tree.name(self.selected).into_owned();
        let size = tree.node(self.selected).size_in(self.mode);
        let result: Slot<Result<(), String>> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-delete".into())
            .spawn(move || put(&worker_result, actions::delete(&path)));
        if let Err(err) = spawned {
            ui.set_status(format!("Could not delete {name}: {err}").into());
            return;
        }
        ui.set_status(format!("Deleting {name}…").into());
        ui.set_can_delete(false);
        self.deletion = Some(RunningDelete {
            node: self.selected,
            name,
            size,
            result,
        });
        self.ensure_ticking();
    }

    /// Takes paths deleted at the Hub's request out of the open scan.
    fn forget_deleted(&mut self, paths: &[PathBuf]) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        let root = tree.path(tree.root());
        let mut gone = Vec::new();
        for path in paths {
            let Ok(below) = path.strip_prefix(&root) else {
                continue;
            };
            let mut id = Some(tree.root());
            for part in below.components() {
                id = id.and_then(|id| tree.child_by_name(id, part.as_os_str()));
            }
            if let Some(id) = id
                && id != tree.root()
            {
                tree.mark_deleted(id);
                gone.push(id);
            }
        }
        if gone.is_empty() {
            return;
        }
        self.basket
            .retain(|&b| !tree.ancestors(b).any(|a| gone.contains(&a)));
        let selected_gone = tree.ancestors(self.selected).find(|a| gone.contains(a));
        let expanded = self.rows.expanded_nodes();
        self.rows.rebuild(tree, self.mode, &expanded);
        match selected_gone {
            Some(id) => {
                let parent = tree.node(id).parent;
                self.focus = NO_NODE;
                self.select(parent as i32, false);
            }
            None => self.refresh_selection(),
        }
        self.refresh_cleanup();
    }

    fn finish_delete(&mut self, deletion: RunningDelete, result: Result<(), String>) {
        let Some(ui) = self.ui() else {
            return;
        };
        let Some(tree) = &mut self.tree else {
            return;
        };
        if let Err(err) = result {
            ui.set_status(format!("Could not delete {}: {err}", deletion.name).into());
            self.refresh_selection();
            return;
        }

        tree.mark_deleted(deletion.node);
        let gone = deletion.node;
        self.basket
            .retain(|&b| !tree.ancestors(b).any(|a| a == gone));
        let parent = tree.node(deletion.node).parent;
        let expanded = self.rows.expanded_nodes();
        self.rows.rebuild(tree, self.mode, &expanded);
        // Show the folder the item was deleted from.
        self.focus = NO_NODE;
        self.select(parent as i32, false);
        self.refresh_cleanup();
        ui.set_status(
            format!(
                "Moved {} ({}) to the quarantine; empty it to free the space.",
                deletion.name,
                format_size(deletion.size)
            )
            .into(),
        );
        self.quarantine_changed();
    }

    fn grant_permission(&mut self) {
        #[cfg(target_os = "android")]
        {
            crate::android::request_storage_access();
            if self.awaiting_permission.is_none() {
                self.awaiting_permission = Some(String::new());
            }
            self.ensure_ticking();
        }
    }
}

/// Makes `model` hold `rows`, changing only the rows that differ so lists
/// keep their scroll position.
fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    for (i, row) in rows.iter().enumerate() {
        if i >= model.row_count() {
            model.push(row.clone());
        } else if model.row_data(i).as_ref() != Some(row) {
            model.set_row_data(i, row.clone());
        }
    }
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
}

/// Whether FreeSize can read and write all of shared storage.
fn storage_access() -> bool {
    #[cfg(target_os = "android")]
    return crate::android::has_storage_access();
    #[cfg(not(target_os = "android"))]
    true
}

/// `"1 file"`, `"1,234 files"`.
fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", format_count(n), if n == 1 { one } else { many })
}

/// Path of `to` relative to its ancestor `from`, e.g. `Videos/2023`.
fn relative_path(tree: &Tree, from: NodeId, to: NodeId) -> String {
    let mut names: Vec<_> = tree
        .ancestors(to)
        .take_while(|&n| n != from)
        .map(|n| tree.name(n).into_owned())
        .collect();
    names.reverse();
    names.join(MAIN_SEPARATOR_STR)
}

/// Fill and text color for a treemap block. Each top-level folder gets its
/// own hue; deeper levels and files are lighter.
fn block_color(group: u16, depth: u8, is_dir: bool) -> (Color, Color) {
    const HUES: [f32; 10] = [
        210.0, 145.0, 35.0, 355.0, 275.0, 185.0, 320.0, 85.0, 245.0, 15.0,
    ];
    let hue = HUES[group as usize % HUES.len()];
    let lightness = if is_dir { 0.40 } else { 0.55 } + 0.07 * f32::from(depth.min(4));
    let lightness = lightness.min(0.82);
    let fill = hsl(hue, 0.55, lightness);
    let text = if lightness > 0.58 {
        Color::from_rgb_u8(0x1b, 0x1b, 0x1f)
    } else {
        Color::from_rgb_u8(0xff, 0xff, 0xff)
    };
    (fill, text)
}

fn extension_color(ext: &str) -> Color {
    // FNV-1a, so each extension keeps its color between runs.
    let hash = ext.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    hsl((hash % 360) as f32, 0.6, 0.55)
}

fn hsl(hue: f32, saturation: f32, lightness: f32) -> Color {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    let to_u8 = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::from_rgb_u8(to_u8(r), to_u8(g), to_u8(b))
}

/// `YYYY-MM-DD` in UTC for a Unix timestamp.
fn format_date(secs: i64) -> Option<String> {
    if secs <= 0 {
        return None;
    }
    // Howard Hinnant's civil_from_days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// "put back 2 items" → "Put back 2 items".
fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

/// Whether Clean junk lists the phone's junk (the apps report's) rather
/// than the desktop's known places: on phones, and in the fake-apps demo.
fn phone_junk() -> bool {
    cfg!(target_os = "android") || apps::demo_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(format_date(0), None);
        assert_eq!(format_date(86_400).as_deref(), Some("1970-01-02"));
        assert_eq!(format_date(951_782_400).as_deref(), Some("2000-02-29"));
        assert_eq!(format_date(1_760_140_800).as_deref(), Some("2025-10-11"));
    }

    #[test]
    fn colors_are_stable_and_valid() {
        assert_eq!(extension_color("mkv"), extension_color("mkv"));
        let (fill, _) = block_color(3, 1, true);
        assert_eq!(fill.alpha(), 255);
        assert_eq!(hsl(0.0, 1.0, 0.5), Color::from_rgb_u8(255, 0, 0));
        assert_eq!(hsl(120.0, 1.0, 0.5), Color::from_rgb_u8(0, 255, 0));
    }
}

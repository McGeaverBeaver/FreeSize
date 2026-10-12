//! Application state and the logic behind every UI callback.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{MAIN_SEPARATOR_STR, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use disktree_core::bubbles::{self, BubbleOptions};
use disktree_core::snapshot::{self, Snapshot};
use disktree_core::treemap::{self, Rect, TreemapOptions};
use disktree_core::{
    DuplicateGroup, DuplicateProgress, Kind, NO_NODE, NodeId, ScanOptions, ScanProgress,
    ScanResult, SizeMode, Tree, disk_space, duplicate_candidates, extension_stats, format_count,
    format_size, has_extension, kinds, largest_files, largest_files_where, rebuildable, scan,
};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel,
};

use crate::appbubbles;
use crate::apps::{self, AppReport};
use crate::changes;
use crate::cleanup::JunkKind;
use crate::filecleanup::{self, Duplicates};
use crate::growth::{self, DriveLog};
use crate::home::{self, AppsState, sheet};
use crate::insights::View;
use crate::owners::Owners;
use crate::rows::{self, TreeRows};
use crate::scans;
use crate::{
    AppWindow, BubbleItem, CleanRow, Crumb, FileRow, HomeRow, MapRect, SheetAction, TypeRow,
    actions, locations,
};

/// Icons beyond the kinds of content (see `Icons.of` in the UI).
const ICON_REBUILD: i32 = 12;
const ICON_MORE: i32 = 13;
/// Most files listed when the list is limited to one type.
const MAX_OF_TYPE: usize = 1000;

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
}

/// Waits for the user to come back from a system screen (uninstall dialog,
/// app info, clear cache) so the app sizes can be measured again.
struct ReturnWatch {
    left: bool,
    ticks: u64,
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
    cleanup_found: Vec<CleanRow>,
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
    /// The app group shown as bubbles, zoomed into an app if set.
    app_bubbles: bool,
    app_zoom: Option<String>,
    app_bubble_size: (f32, f32),
    app_bubble_rows: Rc<VecModel<BubbleItem>>,
    /// The file type the files list is limited to.
    type_filter: Option<String>,
    /// Folder sizes of earlier scans of the scanned folder, oldest first,
    /// and the one the Changes tab compares with.
    earlier: Vec<Snapshot>,
    compare: Option<usize>,
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
    /// Which group of apps the home screen lists.
    view: View,
    sheet: Option<SheetTarget>,
    return_watch: Option<ReturnWatch>,
    apk_job: Option<Slot<Result<String, String>>>,
    /// Junk waiting for the user to confirm deleting it.
    pending_cleanup: Option<JunkKind>,
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
    /// Past sizes of the drives on the start screen.
    drive_log: DriveLog,
    /// A file to open once Android has indexed it, and how many times that
    /// was tried.
    pending_open: Option<(PathBuf, u32)>,
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

/// Creates the controller and connects it to `ui`. Keep the returned value
/// alive for as long as the window runs.
pub fn install(ui: &AppWindow, initial: Option<PathBuf>) -> Rc<RefCell<Controller>> {
    let ctl = Rc::new_cyclic(|me| {
        RefCell::new(Controller {
            me: me.clone(),
            ui: ui.as_weak(),
            tree: None,
            rows: Rc::new(TreeRows::default()),
            home_rows: Rc::new(VecModel::default()),
            cleanup_rows: Rc::new(VecModel::default()),
            cleanup_found: Vec::new(),
            mode: SizeMode::Allocated,
            selected: 0,
            focus: 0,
            map_size: (0.0, 0.0),
            bubble_size: (0.0, 0.0),
            kinds: Vec::new(),
            rebuildable: HashSet::new(),
            owners: Owners::default(),
            app_bubbles: false,
            app_zoom: None,
            app_bubble_size: (0.0, 0.0),
            app_bubble_rows: Rc::new(VecModel::default()),
            type_filter: None,
            earlier: Vec::new(),
            compare: None,
            change_rows: Rc::new(VecModel::default()),
            summary: None,
            scan: None,
            deletion: None,
            awaiting_permission: None,
            apps: AppsState::Unsupported,
            apps_job: None,
            awaiting_usage_access: false,
            view: View::All,
            sheet: None,
            return_watch: None,
            apk_job: None,
            pending_cleanup: None,
            cleanup_job: None,
            cache_check: None,
            cache_freed: None,
            remeasure: false,
            dupes: DupeSearch::Idle,
            drive_log: DriveLog::default(),
            pending_open: None,
            timer: Timer::default(),
            ticks: 0,
        })
    });

    ui.set_tree_rows(ModelRc::from(ctl.borrow().rows.clone()));
    ui.set_home_rows(ModelRc::from(ctl.borrow().home_rows.clone()));
    ui.set_cleanup_rows(ModelRc::from(ctl.borrow().cleanup_rows.clone()));
    ui.set_app_bubbles(ModelRc::from(ctl.borrow().app_bubble_rows.clone()));
    ui.set_change_rows(ModelRc::from(ctl.borrow().change_rows.clone()));
    let start = initial.clone().unwrap_or_else(locations::default_path);
    ui.set_path(start.to_string_lossy().as_ref().into());
    ui.set_can_browse(actions::CAN_BROWSE);
    ui.set_can_reveal(actions::CAN_REVEAL);
    ui.set_delete_label(actions::DELETE_LABEL.into());
    ui.set_status("Pick a drive or folder, or type a path and press Scan.".into());

    let weak = Rc::downgrade(&ctl);
    on!(ui, on_start_scan, weak, |c, path| c
        .start_scan(path.to_string()));
    on!(ui, on_cancel_scan, weak, |c| c.cancel_scan());
    on!(ui, on_show_locations, weak, |c| c.show_locations());
    on!(ui, on_toggle_row, weak, |c, row| c.toggle_row(row));
    on!(ui, on_select_node, weak, |c, id, reveal| c
        .select(id, reveal));
    on!(ui, on_map_resized, weak, |c, w, h| c.map_resized(w, h));
    on!(ui, on_bubbles_resized, weak, |c, w, h| c
        .bubbles_resized(w, h));
    on!(ui, on_map_view_changed, weak, |c| c.update_map());
    on!(ui, on_app_bubble, weak, |c, key| c.app_bubble(key.as_str()));
    on!(ui, on_type_picked, weak, |c, key| c.pick_type(key.as_str()));
    on!(ui, on_clear_files_filter, weak, |c| c.clear_type_filter());
    on!(ui, on_compare_scan, weak, |c, index| c.compare_with(index));
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
    on!(ui, on_sheet_action, weak, |c, id| c
        .sheet_action(id.as_str()));
    on!(ui, on_cleanup_action, weak, |c| c.cleanup_action());

    // The folder picker is modal, so run it without holding the controller.
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
        c.record_drives();
        if apps::supported() {
            if apps::has_access() {
                c.start_apps_job();
            } else {
                c.apps = AppsState::NeedsAccess { asked: false };
            }
        }
        c.refresh_home();
    }

    if let Some(path) = initial {
        ctl.borrow_mut()
            .start_scan(path.to_string_lossy().into_owned());
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
            if let AppsState::Ready(report) = &self.apps {
                // Folders of apps in Android/media and the like get their icons.
                self.owners.set_android_icons(report.icons.clone());
                self.update_map();
            }
            if self.remeasure {
                self.remeasure = false;
                self.start_apps_job();
            } else if let (Some((before, true)), Some(report)) = (self.cache_check, self.report()) {
                let after: u64 = report.apps.iter().map(|a| a.cache_bytes).sum();
                let freed = before.saturating_sub(after);
                self.cache_check = None;
                self.cache_freed = Some(freed);
                self.set_status(&format!(
                    "Android's cache cleaner freed {}.",
                    format_size(freed)
                ));
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
                let expired = (!watch.left && watch.ticks > 300) || watch.ticks > 6000;
                if back || expired {
                    self.return_watch = None;
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
            let mut text = format!("Freed {}.", format_size(freed));
            if !errors.is_empty() {
                text.push_str(&format!(
                    " {} couldn't be deleted, for example {}",
                    plural(errors.len() as u64, "item", "items"),
                    errors[0]
                ));
            }
            self.set_status(&text);
            self.start_apps_job();
        }

        if let DupeSearch::Running { result, .. } = &self.dupes {
            if let Some(groups) = take(result) {
                self.dupes = DupeSearch::Done(groups);
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

        if let Some(job) = &self.apk_job
            && let Some(result) = take(job)
        {
            self.apk_job = None;
            if let Some(ui) = self.ui() {
                ui.set_status(result.unwrap_or_else(|err| err).into());
            }
        }

        let busy = self.scan.is_some()
            || self.deletion.is_some()
            || self.awaiting_permission.is_some()
            || self.apps_job.is_some()
            || self.awaiting_usage_access
            || self.return_watch.is_some()
            || self.apk_job.is_some()
            || self.cleanup_job.is_some()
            || matches!(self.dupes, DupeSearch::Running { .. })
            || self.pending_open.is_some();
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
        let spawned = {
            let (root, progress, result) = (root.clone(), progress.clone(), result.clone());
            std::thread::Builder::new()
                .name("disktree-scan".into())
                .spawn(move || {
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
                        Scanned {
                            result: r,
                            kinds,
                            earlier,
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
        self.scan = Some(RunningScan {
            path: root,
            progress,
            result,
        });
        ui.set_path(path.as_str().into());
        ui.set_scan_title(format!("Scanning {path}").into());
        ui.set_scan_counts("Starting…".into());
        ui.set_scan_current(SharedString::new());
        ui.set_status("Scanning…".into());
        ui.set_scanning(true);
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
                self.compare = self.earlier.len().checked_sub(1);
                scanned.result
            }
            Err(err) => {
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
        self.type_filter = None;
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
        self.refresh_focus_views();
        self.refresh_selection();
        self.refresh_status();
        self.refresh_cleanup();
    }

    fn show_locations(&mut self) {
        let Some(ui) = self.ui() else {
            return;
        };
        self.stop_duplicates();
        self.tree = None;
        self.summary = None;
        self.rows.clear();
        ui.set_map_rects(ModelRc::default());
        ui.set_largest_files(ModelRc::default());
        ui.set_file_types(ModelRc::default());
        self.cleanup_found.clear();
        self.cleanup_rows.clear();
        self.change_rows.clear();
        self.earlier.clear();
        self.compare = None;
        ui.set_has_tree(false);
        ui.set_status("Pick a drive or folder, or type a path and press Scan.".into());
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
        let options = home::Options {
            view: self.view,
            refreshing: self.apps_job.is_some() && matches!(self.apps, AppsState::Ready(_)),
            storage_access: storage_access(),
            abilities: apps::abilities(),
            cache_freed: self.cache_freed,
            drives: Some(&self.drive_log),
            bubbles: self.app_bubbles,
            zoom: self.app_zoom.as_deref(),
        };
        sync_model(&self.home_rows, home::rows(&self.apps, options));
        self.update_app_bubbles(false);
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
        let Some(report) = self.report().filter(|_| self.app_bubbles) else {
            self.app_bubble_rows.clear();
            return;
        };
        if width < 40.0 || height < 40.0 {
            return;
        }
        let items: Vec<BubbleItem> =
            appbubbles::bubbles(report, self.view, self.app_zoom.as_deref(), width, height)
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
            .name("disktree-apps".into())
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
        match id {
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
                let before: u64 = self
                    .report()
                    .map_or(0, |r| r.apps.iter().map(|a| a.cache_bytes).sum());
                if apps::clear_all_cache() {
                    self.cache_check = Some((before, false));
                    self.watch_return();
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
            home::ZOOM_OUT => {
                self.app_zoom = None;
                self.refresh_home();
                self.update_app_bubbles(true);
            }
            _ => {
                if let Some(view) = id.strip_prefix(home::VIEW_PREFIX).and_then(View::from_id) {
                    self.view = view;
                    self.app_zoom = None;
                    self.refresh_home();
                    self.update_app_bubbles(true);
                } else if let Some(kind) = id
                    .strip_prefix(home::CLEAN_PREFIX)
                    .and_then(JunkKind::from_id)
                {
                    self.request_cleanup(kind);
                }
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
        self.sheet = Some(target);
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
        }
    }

    /// Copies an app's APK to Download/DiskTree on a worker thread.
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
            .name("disktree-apk".into())
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
            .name("disktree-cleanup".into())
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
        self.return_watch = Some(ReturnWatch {
            left: false,
            ticks: 0,
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
            "{} in {} and {} · scanned in {:.1}s",
            format_size(root.size_in(self.mode)),
            plural(u64::from(root.files), "file", "files"),
            plural(u64::from(root.dirs), "folder", "folders"),
            summary.seconds
        );
        if summary.errors > 0 {
            status.push_str(&format!(
                " · {} could not be read",
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
        ui.set_can_delete(id != tree.root() && self.deletion.is_none());
        // Android has no file manager to open a folder in.
        ui.set_can_open(!(cfg!(target_os = "android") && node.is_dir()));
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
        let rows = match self.compare.and_then(|i| self.earlier.get(i)) {
            Some(earlier) => changes::rows(tree, &self.root_label(tree), earlier, self.mode, now),
            None => vec![CleanRow {
                kind: 0,
                id: -1,
                title: "No earlier scan of this folder".into(),
                subtitle: "DiskTree keeps the folder sizes of every scan. Scan this folder again \
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
        self.cleanup_found = filecleanup::found_rows(tree, self.mode, apps::now() as i64);
        self.show_cleanup();
        self.refresh_changes();
    }

    fn show_cleanup(&self) {
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
        let mut rows = self.cleanup_found.clone();
        rows.extend(filecleanup::duplicate_rows(tree, duplicates));
        sync_model(&self.cleanup_rows, rows);
    }

    /// The Duplicate files button: starts the search, or stops it while it
    /// runs.
    fn cleanup_action(&mut self) {
        if matches!(self.dupes, DupeSearch::Running { .. }) {
            self.stop_duplicates();
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
                .name("disktree-duplicates".into())
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
        // Bars compare a listed file with all files of its type when the
        // list shows one type, else with the whole folder.
        let total = self
            .type_filter
            .as_ref()
            .and_then(|ext| stats.iter().find(|s| &s.extension == ext))
            .map_or(folder_total, |s| s.size.max(1) as f64);

        let found = match &self.type_filter {
            Some(ext) => largest_files_where(tree, self.focus, self.mode, MAX_OF_TYPE, |id| {
                has_extension(tree, id, ext)
            }),
            None => largest_files(tree, self.focus, self.mode, 100),
        };
        ui.set_files_filter(match &self.type_filter {
            Some(ext) => {
                let name = if ext.is_empty() {
                    "Files without an extension".to_string()
                } else {
                    format!("Only .{ext}")
                };
                let (count, bytes) = stats
                    .iter()
                    .find(|s| &s.extension == ext)
                    .map_or((0, 0), |s| (s.files, s.size));
                let mut text = format!(
                    "{name} · {}, {}",
                    plural(count, "file", "files"),
                    format_size(bytes)
                );
                if count > MAX_OF_TYPE as u64 {
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

        let types: Vec<TypeRow> = stats
            .into_iter()
            .take(60)
            .map(|stat| TypeRow {
                key: stat.extension.as_str().into(),
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
            .collect();
        ui.set_file_types(ModelRc::new(VecModel::from(types)));
    }

    /// Lists the files of one type, from the file types list.
    fn pick_type(&mut self, extension: &str) {
        self.type_filter = Some(extension.to_string());
        self.update_lists();
        if let Some(ui) = self.ui() {
            // Show the files: the "Largest files" tab, or "Files" on phones.
            ui.set_wide_tab(1);
            ui.set_compact_tab(2);
        }
    }

    fn clear_type_filter(&mut self) {
        self.type_filter = None;
        self.update_lists();
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
        let text = if cfg!(target_os = "android") {
            format!("Permanently delete {what}? This cannot be undone.")
        } else if cfg!(windows) {
            format!("Move {what} to the Recycle Bin?")
        } else {
            format!("Move {what} to the Trash?")
        };
        ui.set_confirm_text(text.into());
        ui.set_confirm_open(true);
    }

    fn cancel_delete(&mut self) {
        self.pending_cleanup = None;
        if let Some(ui) = self.ui() {
            ui.set_confirm_open(false);
            ui.set_delete_label(actions::DELETE_LABEL.into());
        }
    }

    fn confirm_delete(&mut self) {
        if let Some(kind) = self.pending_cleanup.take() {
            if let Some(ui) = self.ui() {
                ui.set_confirm_open(false);
                ui.set_delete_label(actions::DELETE_LABEL.into());
            }
            self.run_cleanup(kind);
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
            .name("disktree-delete".into())
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
        let parent = tree.node(deletion.node).parent;
        let expanded = self.rows.expanded_nodes();
        self.rows.rebuild(tree, self.mode, &expanded);
        // Show the folder the item was deleted from.
        self.focus = NO_NODE;
        self.select(parent as i32, false);
        self.refresh_cleanup();
        ui.set_status(
            format!(
                "Deleted {} and freed {}",
                deletion.name,
                format_size(deletion.size)
            )
            .into(),
        );
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

/// Whether DiskTree can read and write all of shared storage.
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

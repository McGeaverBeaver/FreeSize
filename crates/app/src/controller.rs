//! Application state and the logic behind every UI callback.

use std::cell::RefCell;
use std::io;
use std::path::{MAIN_SEPARATOR_STR, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use disktree_core::treemap::{self, Rect, TreemapOptions};
use disktree_core::{
    NO_NODE, NodeId, ScanOptions, ScanProgress, ScanResult, SizeMode, Tree, extension_stats,
    format_count, format_size, largest_files, scan,
};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::apps::{self, AppReport};
use crate::home::{self, AppsState};
use crate::rows::{self, TreeRows};
use crate::{AppWindow, FileRow, HomeRow, MapRect, TypeRow, actions, locations};

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
    result: Slot<io::Result<ScanResult>>,
}

struct RunningDelete {
    node: NodeId,
    name: String,
    size: u64,
    result: Slot<Result<(), String>>,
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
    mode: SizeMode,
    selected: NodeId,
    /// The folder shown in the treemap and the file lists.
    focus: NodeId,
    map_size: (f32, f32),
    summary: Option<ScanSummary>,
    scan: Option<RunningScan>,
    deletion: Option<RunningDelete>,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    awaiting_permission: Option<String>,
    apps: AppsState,
    apps_job: Option<Slot<Result<AppReport, String>>>,
    /// Waiting for the user to come back from the Usage access screen.
    awaiting_usage_access: bool,
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
            mode: SizeMode::Allocated,
            selected: 0,
            focus: 0,
            map_size: (0.0, 0.0),
            summary: None,
            scan: None,
            deletion: None,
            awaiting_permission: None,
            apps: AppsState::Unsupported,
            apps_job: None,
            awaiting_usage_access: false,
            timer: Timer::default(),
            ticks: 0,
        })
    });

    ui.set_tree_rows(ModelRc::from(ctl.borrow().rows.clone()));
    ui.set_home_rows(ModelRc::from(ctl.borrow().home_rows.clone()));
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
    on!(ui, on_size_mode_changed, weak, |c| c.size_mode_changed());
    on!(ui, on_go_up, weak, |c| c.go_up());
    on!(ui, on_reveal_selected, weak, |c| c.reveal_selected());
    on!(ui, on_delete_requested, weak, |c| c.request_delete());
    on!(ui, on_delete_confirmed, weak, |c| c.confirm_delete());
    on!(ui, on_delete_cancelled, weak, |c| c.cancel_delete());
    on!(ui, on_grant_permission, weak, |c| c.grant_permission());
    on!(ui, on_open_app, weak, |c, package| c
        .open_app(package.as_str()));
    on!(ui, on_home_action, weak, |c, id| c.home_action(id.as_str()));

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
            self.refresh_home();
        }

        if self.awaiting_usage_access && self.ticks.is_multiple_of(10) && apps::has_access() {
            self.awaiting_usage_access = false;
            self.start_apps_job();
        }

        let busy = self.scan.is_some()
            || self.deletion.is_some()
            || self.awaiting_permission.is_some()
            || self.apps_job.is_some()
            || self.awaiting_usage_access;
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
        let result: Slot<io::Result<ScanResult>> = Arc::default();
        let spawned = {
            let (root, progress, result) = (root.clone(), progress.clone(), result.clone());
            std::thread::Builder::new()
                .name("disktree-scan".into())
                .spawn(move || put(&result, scan(&root, &ScanOptions::default(), &progress)))
        };
        if let Err(err) = spawned {
            ui.set_status(format!("Could not start scanning: {err}").into());
            return;
        }

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

    fn finish_scan(&mut self, path: PathBuf, result: io::Result<ScanResult>) {
        let Some(ui) = self.ui() else {
            return;
        };
        ui.set_scanning(false);
        let result = match result {
            Ok(result) => result,
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
        let tree = self.tree.insert(result.tree);
        self.selected = tree.root();
        self.focus = tree.root();
        self.rows.rebuild(tree, self.mode, &[tree.root()]);
        ui.set_tree_scroll(0.0);
        ui.set_has_tree(true);
        self.refresh_focus_views();
        self.refresh_selection();
        self.refresh_status();
    }

    fn show_locations(&mut self) {
        let Some(ui) = self.ui() else {
            return;
        };
        self.tree = None;
        self.summary = None;
        self.rows.clear();
        ui.set_map_rects(ModelRc::default());
        ui.set_largest_files(ModelRc::default());
        ui.set_file_types(ModelRc::default());
        ui.set_has_tree(false);
        ui.set_status("Pick a drive or folder, or type a path and press Scan.".into());
        // Free space and app sizes may have changed since the last visit.
        if matches!(self.apps, AppsState::Ready(_) | AppsState::Failed(_)) {
            self.start_apps_job();
        }
        self.refresh_home();
    }

    /// Updates the home screen in place: only rows that differ change, so
    /// the list keeps its scroll position and the measured heights of rows
    /// that stay the same.
    fn refresh_home(&self) {
        let rows = home::rows(&self.apps);
        let model = &self.home_rows;
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

    // ----- Apps -----------------------------------------------------------

    /// Measures every app's storage on a worker thread.
    fn start_apps_job(&mut self) {
        if self.apps_job.is_some() {
            return;
        }
        let result: Slot<Result<AppReport, String>> = Arc::default();
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("disktree-apps".into())
            .spawn(move || put(&worker_result, apps::collect()));
        self.apps = match spawned {
            Ok(_) => {
                self.apps_job = Some(result);
                self.ensure_ticking();
                AppsState::Loading
            }
            Err(err) => AppsState::Failed(err.to_string()),
        };
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
            _ => {}
        }
    }

    fn open_app(&mut self, package: &str) {
        apps::open(package);
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
        ui.set_can_go_up(self.focus != tree.root());
    }

    // ----- Treemap and lists ----------------------------------------------

    fn map_resized(&mut self, width: f32, height: f32) {
        if (width, height) != self.map_size {
            self.map_size = (width, height);
            self.update_treemap();
        }
    }

    fn refresh_focus_views(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let focus = tree.node(self.focus);
        let label = if self.focus == tree.root() {
            tree.path(self.focus).to_string_lossy().into_owned()
        } else {
            relative_path(tree, tree.root(), self.focus)
        };
        ui.set_focus_label(format!("{label} — {}", format_size(focus.size_in(self.mode))).into());
        self.update_treemap();
        self.update_lists();
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
                }
            })
            .collect();
        ui.set_map_rects(ModelRc::new(VecModel::from(rects)));
    }

    fn update_lists(&self) {
        let (Some(ui), Some(tree)) = (self.ui(), &self.tree) else {
            return;
        };
        let total = tree.node(self.focus).size_in(self.mode).max(1) as f64;

        let files: Vec<FileRow> = largest_files(tree, self.focus, self.mode, 100)
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

        let types: Vec<TypeRow> = extension_stats(tree, self.focus, self.mode)
            .into_iter()
            .take(60)
            .map(|stat| TypeRow {
                label: if stat.extension.is_empty() {
                    "(no extension)".into()
                } else {
                    format!(".{}", stat.extension).into()
                },
                size: format_size(stat.size).into(),
                files: plural(stat.files, "file", "files").into(),
                fraction: (stat.size as f64 / total) as f32,
                color: extension_color(&stat.extension),
            })
            .collect();
        ui.set_file_types(ModelRc::new(VecModel::from(types)));
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
        if let Some(ui) = self.ui() {
            ui.set_confirm_open(false);
        }
    }

    fn confirm_delete(&mut self) {
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

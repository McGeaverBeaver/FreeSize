# Architecture

FreeSize is a Cargo workspace with six crates:

```
crates/core   freesize-core   scanner, tree model, stats, treemap and bubble
                              layout, snapshots, scan files
crates/cli    freesize-cli    command-line front end and the Hub agent
crates/app    freesize-app    Slint UI for desktop and Android
crates/proto  freesize-proto  keys, signatures, pairing links and messages
                              shared by devices and the Hub
crates/agent  freesize-agent  the device side of the Hub
crates/hub    freesize-hub    FreeSize Hub: web server, dashboard, database
```

All the scanning logic lives in `core`, which has no UI or platform UI
dependencies, so it can be tested on its own. The CLI and the app are thin
layers on top of it. The Hub (see [Hub](#hub)) adds a server that collects
what devices report.

## Core

### Tree model (`tree.rs`)

A scan of a large drive can produce tens of millions of entries, so the tree
is stored compactly:

- Every entry is a `Node` in one `Vec<Node>`, referred to by a `u32` index.
- All names live in one shared byte buffer. Each node stores an offset and a
  length into it.
- **The children of a directory are always contiguous**, so a node needs only
  `first_child` and `child_count`, not sibling pointers.
- **Children always have a higher index than their parent.** That makes the
  totals one reverse pass over the array (`Tree::aggregate`): by the time a
  directory is reached, everything below it has already been added in.

A node is 56 bytes plus its name, roughly 70 bytes per entry in practice. A
`PathBuf` per entry would need several times that.

Names are stored as `OsStr::as_encoded_bytes`, so paths round-trip exactly,
including non-UTF-8 names on Linux and unpaired surrogates on Windows. Display
uses a lossy conversion.

Each directory stores subtree totals: logical size, allocated size, file count,
folder count and newest modification time. `mark_deleted` subtracts a node's
totals from its ancestors, so the UI can update after a delete without
rescanning.

### Scanner (`scan.rs`)

```
            jobs (dir id + path)
 builder  ───────────────────────▶  N worker threads
 (owns    ◀───────────────────────  read_dir + metadata
  Tree)     listings (one per dir)
```

- **Workers** pull a directory from a channel, list it and read every entry's
  metadata. They send back one `Listing` with all entry names in a single
  buffer.
- The **builder** (the calling thread) owns the `Tree`. It appends each listing
  as one contiguous block of children, then sends the subdirectories back as
  new jobs. A pending-job counter tells it when the scan is done.
- Only the builder touches the tree, so there is no locking on the hot path.
- A panic in a worker is turned into an error listing. Without that, the
  builder would wait forever for a directory that never comes back.
- Progress is a set of atomic counters (`ScanProgress`) that the UI polls.
  Cancelling sets a flag; workers then return empty listings, so the scan
  finishes quickly with a partial tree.
- The default thread count is 2× the number of cores, clamped to 4–32,
  because scanning mostly waits on I/O.

Rules shared by every platform:

- **Symlinks and junctions** are recorded but never followed.
- **Mount points**: directories on another filesystem are flagged and skipped
  unless `one_filesystem` is off.
- **Hard links**: a file with more than one link is counted only the first
  time its `(device, inode)` is seen.
- **Errors**: unreadable entries are flagged and counted; they never abort the
  scan. Only an unreadable root fails it. `Tree::unreadable` lists the
  folders that couldn't be listed, which is what a scan with more rights
  would add.

### Rights (`access.rs`)

`rights()` says whose eyes a scan sees with: the user's, an elevated
administrator's (Windows, from the token's elevation) or root's (Unix,
`geteuid`). Reports carry its label so the Hub can say a Windows agent
sees only what its user sees. `enable_backup_privilege()` turns on
`SeBackupPrivilege` for the process; administrators hold it, and with it
on, listing a folder succeeds whatever its permissions say. `beyond_reach`
names what no right reaches on Windows (System Volume Information, the
reserved storage).

### Platform specifics (`platform.rs`, `space.rs`)

| | Allocated size | Extras |
|---|---|---|
| Unix (Linux, Android, macOS) | `st_blocks × 512` | `st_dev` for mount points, `st_nlink`/`st_ino` for hard links |
| macOS | as Unix | `SF_DATALESS` flag marks iCloud files that aren't downloaded |
| Windows | Size rounded up to the volume's cluster size; `GetCompressedFileSizeW` for compressed and sparse files | `RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN` and `OFFLINE` attributes mark cloud placeholders (OneDrive), which count as 0 bytes on disk |

On Windows the metadata comes free with `FindNextFileW`, so the scan costs
about one system call per directory rather than one per file. It never opens a
file, so OneDrive placeholders are never downloaded.

`disk_space` wraps `statvfs` and `GetDiskFreeSpaceExW` to show free and total
space on the start screen.

### Stats and treemap

- `largest_files` keeps a size-`n` min-heap while walking a subtree;
  `largest_files_where` does the same for files passing a test (one type,
  one year, one size band).
- `extension_stats` groups by lower-cased extension. Dotfiles like `.bashrc`
  count as having no extension.
- `age_bins` sums files by the year they were last changed (this year and
  nine before it, then "before" and "no date"), and `size_bins` by size band.
  Each bin keeps its range, so tapping it filters the files list.
- `treemap::squarify` is the squarified treemap algorithm (Bruls, Huizing &
  van Wijk). It keeps each rectangle's area proportional to its size while
  keeping the rectangles close to square.
- `treemap::layout` nests it a few levels deep. Each directory gets a header
  strip for its label, tiny rectangles are dropped, and the total is capped
  (1500 by default) to keep rendering cheap. Items come out parent-first, so
  drawing them in order puts children on top.

### Kinds and bubbles (`kinds.rs`, `bubbles.rs`)

- `kinds` gives every node a `Kind` for its icon: files by extension, and a
  folder the kind that makes up at least half of its bytes (else a plain
  folder). One reverse pass sums bytes per kind into each folder's parent;
  it runs on the scan thread right after scanning.
- `bubbles::pack` is front-chain circle packing (Wang et al. 2006, as in
  d3-hierarchy): each circle goes next to the pair on the outer chain closest
  to the centre, and is moved along the chain if it would overlap. `fit`
  scales a packed group's bounding box into the view. `layout` draws a
  folder's biggest children (40 at most, each at least 0.4 % of the folder),
  folds the rest into one "more" bubble, and packs the contents of big folder
  bubbles inside them, fitted to an approximate smallest enclosing circle
  (Bădoiu–Clarkson iterations).

### Reclaimable space (`reclaim.rs`)

- `rebuildable` walks folders and matches a table of rules: a folder name,
  optionally a parent name (`Caches` under `Library`) and sibling files that
  must exist (`target` next to `Cargo.toml`). A folder holding `pyvenv.cfg`
  is a virtual environment. Matched folders aren't searched further.
- `old_files` is the largest files modified before a cut-off date.
- `duplicate_candidates` collects files of at least a minimum size, skipping
  hard-link duplicates, cloud placeholders and unreadable entries, and groups
  them by size. It owns plain paths so it can move to a worker thread.
  `DuplicateCandidates::find` then hashes 64 KB from each end of every file
  with the same size (xxh3-128), and only files that still match get a full
  hash. Progress and cancellation go through `DuplicateProgress` atomics.

### System and app caches (`reclaim.rs`)

`system_spots` matches the tree against `SpotRule`s: an absolute path in
which `*` stands for one name (`.../Firefox/Profiles/*/cache2`), what it is,
how to remove it, and a `Safety`: safe, check first, or use the system's own
tool. `default_spot_rules` builds the list for the platform it runs on from
environment variables (`SystemDrive`, `windir`, `LOCALAPPDATA`, the home
folder), so the rules match only where the scan covers those places.
Matching ignores case on Windows and macOS.

### Junk cleaner (`junk.rs`)

The rules behind Clean junk, `freesize junk`, the Hub's Junk tab and the
phone's cards: a static list per `Platform` (Windows, macOS, Linux,
Android, Unraid) of `Rule`s, each with an id, a `Category`, a title and
blurb, whether it `refills`, a `Safety` (safe, check first, use the
tool), the app it belongs to and the process names that mean it's
running, path patterns (templates over `~`, `$LOCAL`, `$ROAMING`,
`$WINDIR`, `$DRIVE`, `$PROGRAMDATA`, `$PF86`, `$VAR`, `$STORAGE`, `$SHARES`
and `$ROOT`, with `*` for one name or part of one), what to `Take`
(the folder's contents, the folder itself, files by extension and age,
or empty folders), names to keep, a minimum age, and a `fixture`: a
made-up folder it must find and one it must not, which one test runs for
every rule under a temporary root. `Env` holds the places for one machine
(`system()` from the environment, `android(storage)`, `under(root)` for
tests), and `find` expands the patterns, sizes what matches
(`Item`: a path, its bytes and files, and whether it's the folder itself
or its entries), skips what an earlier rule took so generic rules at the
end of a list never double count, and marks a finding `running` when one
of its processes is (`running_programs`: `/proc` on Linux, `ps` on macOS,
`tasklist` on Windows). `Finding::ticked` is safe and not running (ticked
at first); `Finding::can_tick` is not running and not "use the tool" (the
person may tick it, "check first" ones then go to the quarantine);
`delete` removes an item and reports what stayed in use. Nothing here
decides to run: callers list, the person ticks. `find_all` adds the
leftovers of removed programs (below) to `find`'s findings, dropping any
a rule already covers; `find_here` is that for this machine.

### Leftovers of removed programs (`leftovers.rs`)

The one junk rule (`program-leftovers`, Apps, check first) that isn't a
pattern: the folders in AppData, `~/Library` and the XDG folders that
belonged to a program that's gone. What's installed comes from the
platform (`installed`): on Linux the `.desktop` files in the XDG data
dirs, Flatpak's and snapd's app folders, dpkg's status file, pacman's
database and `rpm -qa`; on macOS every app bundle Spotlight lists
(`mdfind`) plus the Applications folders, with the bundle id from each
`Info.plist`; on Windows the three Uninstall registry keys through
`reg query` (DisplayName, Publisher, InstallLocation; system components
and patches skipped). The parsers are plain functions with tests. A
folder is matched to a program by `key` (lower case, letters and digits,
without versions, architectures or brackets), by the program's id under
the id-named places (`~/.var/app`, `~/snap`, `~/Library/Containers`,
`Caches`, `Preferences`, `Saved Application State`), by its install
location, and under a vendor folder (`Google\Chrome`) by vendor and
name or by the publisher. Every look records the installed programs and
the matched folders in `Record` (`programs.json` in the data folder, the
one the app and `freesize agent` share); a recorded folder whose program
isn't installed any more is a leftover, named after the program and the
date it was last seen. Id-named folders with no program of that id are
leftovers without a record, but only where the id list was read
completely (`Installed::complete`), never for Apple's own ids, and never
when an installed bundle shares the vendor (an app's helpers). Fewer than
ten installed programs means the list wasn't read and nothing is offered;
nothing changed for 14 days (`AGE_DAYS`) is required of every leftover,
and a list of names shared between programs (`NEVER`) is never offered.
Android and Unraid have none of this; the phone's own leftovers rule
(`Android/media`) is in the app.

### Snapshots (`snapshot.rs`)

A `Snapshot` is the folder sizes of one scan: paths relative to the scanned
folder, logical and allocated size, for the biggest folders (10,000 in the
app). Keeping folders rather than files keeps a snapshot of a whole drive
to about 1 MB of JSON, however many files it holds. `compare` lines two snapshots up by path and
reports folders that grew, shrank, appeared or went away by at least a
minimum. A folder is skipped when one of its children accounts for 80% of
its change, so the list names where the change happened rather than every
folder above it. `find` maps a snapshot path back to a node.

### Folder review (`review.rs`)

Sorts a folder's files for a review: `group_of` by extension through
`Kind` (photos, documents, videos, the rest) and `important` for the ones
worth a look, by extension and name (password vaults, keys and
certificates, wallets, tax and accounting files, email archives, documents
and spreadsheets, and camera photos by name, such as `IMG_1234` and
`PXL_…`, or a `DCIM` folder). `copy_candidates` pairs each file of the
folder with the files of its size elsewhere in the tree, owning the paths so
the reading can run on another thread, and `CopyCandidates::find` confirms
copies by a hash of both ends, then the whole file (`reclaim.rs`'s
hashes), keeping one other copy per file.

### Scan files (`ncdu.rs`)

`export` writes a tree in ncdu's JSON export format (version 1.2): a
directory is an array whose first element describes it, a file an object,
sizes as `asize`/`dsize` with a directory's own size separate from its
contents. Read errors, other file systems, hard links and non-regular files
map to ncdu's `read_error`, `excluded`, `hlnkc` and `notreg`. `import`
reads files from FreeSize, ncdu 1.x and ncdu 2.x's JSON export and builds the
tree the way the scanner does (breadth first, so children stay contiguous),
then aggregates the totals.

### The rename (`renamed.rs`)

DiskTree became FreeSize in 0.8.6. `adopt_dir` takes a data folder named
for FreeSize and, when only the folder of the old name beside it exists
(`DiskTree`, `disktree`, `disktree-hub`), renames that into place, so the
desktop app, the agent, a Hub outside Docker and the Android app history
keep their files. `old_env` lists `DISKTREE_…` variables to set under their
`FREESIZE_…` names where those are unset; the Hub and the command line
apply it before any thread starts. The names inside the protocol (signing
headers and labels), the quarantine folder `.disktree-quarantine` (its
manifests record full paths), the metrics and the webhook header keep the
old name.

## App

### UI (`ui/app.slint`)

The whole interface is one Slint file. Rust feeds it plain structs
(`TreeRow`, `MapRect`, `FileRow`, `TypeRow`, `HomeRow`, `CleanRow`, `SheetAction`)
and receives callbacks.

- **Wide layout** (≥ 760 px): folder tree on the left; the map, largest
  files, file types, Clean up and Changes in tabs on the right.
- **Compact layout** (phones, narrow windows): six tabs, larger touch rows, no
  percent column.
- `compact` is set from a `changed width` handler rather than bound to
  `width`. A binding would make the window's size depend on itself through the
  layout.
- The treemap is a list of absolutely positioned rectangles computed in Rust.
  Its size is reported back through `map-resized` whenever the pane changes.
- Outside a scan the window shows one place at a time (`home::Section`:
  Home, Clean, Apps on phones, Quarantine and Hub on desktops, Settings),
  picked from the `Sidebar` (left, desktops) or the `BottomBar` (phones
  and windows below 760 px). The controller rebuilds the same `HomeRow`
  list for the current place and passes the places as `nav-items`, with
  the counts shown on them (Hub requests waiting, items in the
  quarantine). A scan takes the whole window with its own tabs. Settings
  are `HomeRow`s of kind 9, a drop-down, answered through `home-choice`.
- The home screen and the Clean up tab are scrolling layouts, not list
  views. Slint measures wrapped text as a single line when it computes a
  layout's preferred height, so their scroll height comes from the position
  of an empty marker after the last row instead.
- The storage chart is three filled `Path`s whose SVG commands Rust builds
  (`growth::chart`), drawn over each other so the areas stack.
- Kind icons are SVGs in `ui/icons`, drawn tinted with `colorize`; the
  `Icons` global maps a kind number to one. App icons arrive from Rust as
  images and are drawn as they are.
- `MapView` holds the treemap or `BubblePane` plus the Treemap/Bubbles
  switch. Bubbles are rounded rectangles whose size animates from zero when
  a new layout arrives, so zooming in grows the new bubbles; labels are a
  second layer that doesn't take taps. `Breadcrumb` shows the path to the
  shown folder, each step tappable.

### Controller (`controller.rs`)

`Controller` holds the state: the current tree, size mode, selection, the
focused folder (what the treemap and lists show) and any background job. It is
shared as `Rc<RefCell<Controller>>`. UI callbacks borrow it through
`Controller::run`; if it's already borrowed (a callback fired while Rust was
updating the UI), the job is retried on the next event loop turn instead of
panicking.

Long-running work (scanning, deleting, measuring apps, the duplicate and
similar photo searches, selection jobs) runs on a plain thread and writes its
result into an `Arc<Mutex<Option<T>>>`. A 100 ms Slint timer polls those
results and the scan progress, and stops itself when nothing is pending. That
keeps all UI updates on the UI thread with no `Send` requirements on UI state.

Interaction rules:

- Selecting a folder (in the tree, treemap or lists) makes it the focus: the
  treemap zooms into it and the lists cover it.
- Selecting a file keeps the current focus if the file is inside it.
- Selecting from the treemap or lists expands the tree down to the item and
  scrolls to it.

The scan thread does more than scan: it works out every node's kind, takes
a snapshot and records it (`scans.rs`, per scanned folder in
`scans/<hash>.json` in the data folder, returning the earlier ones: the
last 12 in full, older ones trimmed to their 300 biggest folders with
`Snapshot::floor` saying how small a left-out folder was, thinned to a year
like the Hub's history), and,
when the scan covers a whole drive, asks the system about hidden space. A
path ending in `.json` is read as a scan file instead (`imported`), which
turns off Open, Delete, Select and the Clean up tab's tick boxes.

The Clean up tab is a list of `filecleanup::Section`s: each has a card on
the overview (`CleanSection`), a page of rows, what its "select all" ticks
(`picks`; for duplicates every copy but `filecleanup::keeper`'s, for
similar photos all but the largest), the same per group (`groups`, the
group rows carry their index) and, for duplicates, the types to list one
of. The controller keeps the sections, shows the open page's rows and
`mark_cleanup` ticks rows from the selection (the code calls it the
basket; an item is ticked when it or a folder above it is in it), counts them on the cards and warns on
a set whose every copy is ticked; it runs on every change to the selection,
so ticking from the tab, the selection bar or the review dialog stays in
step. Similar photos are handed to Slint as `PhotoSet`s and laid out as
tiles that wrap to the width. The report flattens the sections back into
headings and rows (`filecleanup::flatten`).

A scan that missed folders gets a card at the top of the Clean up tab
(`filecleanup::unreadable_section`) naming them, with the way to a fuller scan
that `elevate.rs` works out per platform. Windows: `ShellExecuteW` with
`runas` starts `freesize-gui --administrator PATH`, a second window that
turns on the backup privilege, scans PATH and says "administrator" in its
title; the original window stays, since an elevated program doesn't see
the user's mapped drives. macOS: first Full Disk Access, checked by opening
TCC's own database and granted in System Settings (`Privacy_AllFiles`,
opened for the user); then `osascript … with administrator privileges`.
Linux: `pkexec`. Both run `freesize-gui --export FILE PATH` as root, with
no window, into `root-scans/` in the data folder (older files removed
first); the controller polls the child on its timer and opens the file as
a saved scan, so nothing from a root's scan can be opened or deleted. The
window itself never runs as root. Android gets no button: apps' private
data is measured through Usage access instead.

### Scan view extras

- `review.rs` makes a folder review's page of the Clean up tab: a
  `filecleanup::Section` with the key `review`, whose type choices are the
  tabs (`filter_index`, with no "Every type"), cached until the tab, the
  results, the previews or the counts of files change. The controller runs
  the previews (`similar::preview`, the first 400 photos, handed over as
  they're made), Compare and Look inside on threads it polls on its timer;
  Photos is drawn as tiles like similar photos. The words are kept in
  `review.json` in the data folder. The tree's rows have a context menu on
  desktops (not on phones, where a long press would fight the list's
  scrolling), and the selection bar a Review button.
- `changes.rs` turns `snapshot::compare` into the Changes tab's rows. The
  bubble view looks up each folder in the compared snapshot and adds the
  difference to its label when it's at least 5% (and 10 MB).
- `hidden.rs` explains used space that no file accounts for, only for a
  scan of a whole drive (`is_drive_root`: a different device than the
  parent on Unix, `C:\` on Windows). It asks the system's tools and stays
  quiet when one is missing: `zfs list` for the snapshots of the dataset
  mounted there, `btrfs subvolume list -s` and `btrfs qgroup show` (the
  snapshots' exclusive bytes, with quotas on) for btrfs, `vssadmin list
  shadowstorage` for shadow copies, `tmutil listlocalsnapshots` for Time
  Machine, `diskutil apfs listSnapshots` for other APFS snapshots and
  `diskutil info` for purgeable space. `filecleanup.rs` shows the gap only
  when it's at least 1 GB and 2% of the used space.
- `moves.rs` moves the selection's items: the target keeps the path below the
  scanned folder, `rename` within a drive, else copy, compare sizes and
  only then remove the original. An existing target stops that item. The
  `(from, to)` pairs go to `last-move.json` so Put back can reverse them.
- `report.rs` builds the HTML report (self-contained, light and dark) and
  the CSV; `actions::save_export` asks where on desktops (`rfd`) and writes
  to `Download/FreeSize` on Android.
- `similar.rs` finds photos that look alike. A photo's fingerprint is two
  64-bit difference hashes of a 9×9 grid of brightness averages: whether
  each cell is darker than its right neighbour, and than the one below,
  after turning the grid by the EXIF orientation. For a JPEG it first reads
  160 KB and looks for the EXIF preview (APP1 → TIFF → IFD1's
  `JPEGInterchangeFormat`), decoding the whole picture only without one.
  Fingerprints within 12 bits of each other match, and pictures with almost
  no contrast get none. Grouping is greedy in time order: a photo joins the
  group whose first photo is closest, which keeps a slow drift through a
  series from chaining everything together. Previews (112 px) are made on
  the worker for the first 400 photos in groups. Four threads at most, and
  decoding is capped at 256 MB, so 50 MP photos without previews don't
  exhaust a phone's memory.

### Tree rows (`rows.rs`)

`TreeRows` is a custom Slint `Model` over the flattened visible rows.
Expanding or collapsing a folder splices a contiguous block of rows and
reports it as one range change, so the list view never rebuilds. A folder
shows its 500 largest children, and the rest are folded into one
"… N smaller items" row.

### Android (`android.rs`)

`android_main` is the NativeActivity entry point. Storage access goes through
JNI, using the same `jni` 0.22 crate as Slint's Android backend:

- Android 11+: `Environment.isExternalStorageManager()` checks access. If it's
  missing, the app opens `Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION`,
  falling back to the general screen on devices that lack the per-app one.
- Android 10 and older: the classic `READ_EXTERNAL_STORAGE` runtime permission.
- While access is missing, the controller re-checks every second. When access
  is granted, it hides the banner and starts any scan the user asked for.

**Scanned pairing links** (0.15.1): the app's activity is a
`NativeActivity` in `singleTask` mode, so a link scanned while it's
running arrives through `onNewIntent`, which `NativeActivity` drops
(`getIntent` still returns the launch intent). The `freesize://pair` and
`disktree://pair` filter is therefore on `PairLink`, a Java activity that
shows nothing (`Theme.NoDisplay`): it writes the link to `pair-link` in the
files folder, opens the app through its launch intent and finishes. The
controller takes the file (`hub::take_pair_link`, which removes it) at
start and on every Hub tick (500 ms) and opens the connect sheet with the
link; at start it still reads the launch intent's link too.

**The background job** (`background.rs`, 0.14): `HubJob`, one Java class in
`crates/app/android/java`, is a `JobService` Android runs about every 15
minutes (periodic, needs a network, persisted across reboots), scheduled by
the app through JNI once it's paired (`android::schedule_hub_job`, through
the context's class loader, since `FindClass` from a native thread only
sees the system's classes) and cancelled on disconnect. The job calls
`nativeRun` in the same `libfreesize_app.so`, which remembers the job's
context (`android::init_background`) so every `Context` call in `android.rs`
works without the activity, and runs one round: nothing while the app's
own Hub worker is running in the process (`hub::worker_active`); otherwise
the reports kept in `hub-pending/` go out (the app saves there what it
couldn't send, oldest dropped past 20), then a fresh report (info, disks,
quarantine, the Nextcloud folder; the apps and junk every six hours, by a
marker file), then one poll. Commands for the phone are marked shown and
come back as notices; what a grant would run isn't run, it's announced
instead; approvals for other devices become notices too; withdrawn
requests take their notices down. Java shows each notice (channel "hub",
tagged by the request's id so a repeat replaces it, opening the app on
tap) when Android 13+'s `POST_NOTIFICATIONS` was granted, which the app
asks for after pairing. The logic stays in Rust; Java only schedules,
calls and shows.

Storage volumes come from `/storage`: `emulated/0` is internal storage, and the
`XXXX-XXXX` entries are SD cards and USB drives.

Errors and panics go to logcat under the tag `FreeSize`, since stderr goes
nowhere on Android. If Slint can't start (it is already set up in this
process), `android_main` logs the error and returns instead of crashing.

### Icons of apps and games (`owners.rs`, `winicon.rs`)

`Owners` answers "which app does this folder belong to?" for the folders
that get an icon, and caches the image per path:

- Android: `Android/{data,media,obb}/<package>`, with icons the app report
  carries. `android::app_icons` draws each app's icon (adaptive icons
  included) into a 72 px bitmap through `PackageManager.getApplicationIcon`
  and a `Canvas`, once per package and version.
- Steam: a folder in `steamapps/common` is matched to its
  `appmanifest_<id>.acf` by `installdir`; the icon is the hashed `.jpg` in
  `appcache/librarycache/<id>/` (or the older `<id>_icon.jpg`) of the
  library's Steam folder or a usual Steam install.
- Linux: a folder directly in `~/.config`, `~/.cache`, `~/.local/share`,
  `~/.var/app`, `~/snap`, `/opt`, `/usr/share`, `/usr/lib`, `/var/lib` (or a
  dot folder in the home folder) is matched by name to `.desktop` entries
  (file id, the last part of a reverse-DNS id, `StartupWMClass`, `Exec`,
  `Icon`). Icons come from the hicolor theme or `pixmaps`.
- Windows: a folder directly in Program Files, AppData or ProgramData gets
  the icon of the `.exe` inside named most like it (else the biggest, two
  levels deep, skipping uninstallers and updaters). `winicon` extracts it at
  64 px with `SHDefExtractIconW`, `GetIconInfo` and `GetDIBits`, using the
  icon mask when there's no alpha channel.
- macOS: `.app` bundles, and folders in `~/Library` (Application Support,
  Caches, Containers, …) matched to installed apps by bundle id or name;
  the icon is decoded from the bundle's `.icns` (`plist`, `icns` crates).

### App and system storage (`apps.rs`, `home.rs`)

A file scan can't account for most of a phone's used space. Other apps'
private data (`/data/data/<app>`, `Android/data`, `Android/obb`) and installed
code can't be read by any app. Android does report their sizes through
`StorageStatsManager`, which needs the user to grant Usage access
(`PACKAGE_USAGE_STATS`, checked through `AppOpsManager`).

`android::collect_app_report` runs on a worker thread and makes these calls:

| Call | Result |
|---|---|
| `PackageManager.getInstalledApplications` | every app; `QUERY_ALL_PACKAGES` lets Android 11+ list them all |
| `queryStatsForPackage` | each app's code, data and cache bytes |
| `queryExternalStatsForUser` | shared storage total minus app-owned folders, i.e. the user's own files |
| `getTotalBytes` | the advertised capacity |

Each app gets its own JNI local frame, and an app that disappears mid-way is
skipped. For each app it also asks:

| Call | Result |
|---|---|
| `UsageStatsManager.queryUsageStats` | when the app was last on screen (two years back) and its screen time over about a month |
| `getPackageInfo` | install date and version |
| `getInstallSourceInfo` | the installer: Play Store, another store, a downloaded file |
| `getLaunchIntentForPackage` | whether it has an icon |
| `ApplicationInfo.isArchived` (Android 15+) | archived apps, listed with `MATCH_ARCHIVED_PACKAGES` |

Usage history is read once per interval type (yearly, monthly, weekly,
daily) and merged. A single two-year query looks simpler, but when none of
Android's records start that far back, Android answers it from the daily
records alone, which cover about a week; every app used before that then
looked never opened.

Once per report it collects the apps that work without being opened: the
default home screen and browser (`resolveActivity`), SMS and phone apps,
keyboards, accessibility services and notification listeners
(`Settings.Secure`), and device admins.

Android 13+ treats Usage access as a *restricted setting*: for an app
installed from a downloaded APK the switch stays greyed out until the user
picks *Allow restricted settings* from the ⋮ menu of the app's info page.
Apps can't detect that state, so once the user has been sent to the Usage
access screen, the home screen adds a card explaining it, with a button that
opens FreeSize's own app info page.

`AppReport::breakdown` divides the data partition (`statvfs`) the way Android
Settings does:

- Apps: code plus data. Preinstalled apps that were never updated count only
  their data, because their code lives on the system partition.
- Your files: the shared storage figure above.
- System & other: what's left of the used space.
- Free.
- Separately, the advertised capacity minus the partition size is reported as
  Android's own share outside user storage.

`insights.rs` turns these facts into advice, as pure functions with tests:
where an app came from and whether a store can reinstall it, which apps are
safe to suggest, the groups (not opened in 3 months, never opened, not from a
store), and tips for well-known apps.

`history.rs` keeps a record of every app seen, so removed apps can be found
again. `apps::collect` loads it, records the new report and saves it. It is
JSON in `Documents/FreeSize/` (survives uninstalling FreeSize) and in the
app's private storage (works without All files access); both copies are read
and merged.

`cleanup.rs` finds space that can go without touching any app, under the
shared storage root: old `.apk` files in Download (except FreeSize's own
`Download/FreeSize`), `DCIM/.thumbnails`, and `Android/media/<package>`
folders of packages that aren't installed (only when the installed list looks
complete), plus the core junk cleaner's Android rules (old downloads,
screen recordings, empty folders) as cards of their own, under the same
ids the Hub uses. Android's trash comes from MediaStore's Files collection queried
with `QUERY_ARG_MATCH_TRASHED`. The same collection's `owner_package_name`
column says which app saved each file; `media_by_owner` adds those up per app
and picks the folder holding most of them for the Browse button. Deleting
runs on a worker thread after the shared confirmation dialog, into the
quarantine like every delete in the app.

### Quarantine and updates (`quarantine.rs`, `updater.rs`)

Every delete in the app (the selection, junk, a Hub request)
goes through `quarantine.rs`, a thin wrapper over the agent's `Quarantine`
with the app's own `Places`: the quarantine for the disk FreeSize's data
folder is on is `<data>/quarantine`, which the person owns, and other
disks (and Android's shared storage) get `.disktree-quarantine` at their
top as on servers. Batches made here are `a<time>-<n>`; a Hub request's
batch is the request's id, so the dashboard can put it back. The Clean up
tab lists the batches (`CleanRow` kind 3, with Put back and Empty, and
Empty all on the heading, each through the shared confirmation dialog
listing the paths), and so does the Quarantine place on desktops, or Clean
on phones (`quarantine::section_rows`, newest first);
after every change the app reports the quarantine to the Hub. Nothing in
it is emptied on a timer.

A batch card opens (`HomeRow.open_key`, `quarantine::OPEN_PREFIX`) the
explorer page, `QuarantinePane`: `quarantine::View` keeps the batch, the
folders opened inside it and the ticked items, and `View::rows` lists the
items grouped by their `group` (the rule, for a Clean junk batch) or by
the folder they came from, then a folder's own contents (`entry_rows`, at
most 500). Open is hidden for programs and scripts
(`runs_when_opened`). Single items go back or go for good through the
agent's `restore_items` and `empty_items`, with the stored names of the
ticked items (or all); `taken` lists the ones whose original place is
occupied, and the conflict dialog picks a `Conflict`: `KeepBoth` puts it
back as "name (put back).ext", `Replace` moves what's in the way into a
batch of its own (`r<time>`, "replaced by a put back"), `Skip` leaves it.
`move_in_group` adds to an existing batch, so one Clean is one batch.

`junk.rs` is the Clean junk screen on desktops and phones, a full-window
pane (`JunkPane`) with three stages (`Stage`): the list, cleaning and
done. The list starts with a summary card (`headline`, `summary`), then
rows of `JunkRow`: category headings, rules with a tick box and a chevron
(greyed, with a badge, while their program is open), the paths of an open
rule with Show, notes, and last the rules left to the system's tools
(kind `TOOL`, no tick box, not counted) with Open settings and Show
command. A shown command is a `COMMAND` row with the command line, Copy
and Run; `COMMANDS` says per rule what runs, what it changes, how to undo
it and whether it needs an administrator. `run_command` runs it on a
worker thread (through `osascript … with administrator privileges` or
`pkexec` when FreeSize isn't elevated on macOS or Linux; on Windows
"Run as administrator" reopens FreeSize through UAC with `--junk`, which
opens Clean junk), keeps the last lines of output in the row, and looks
again. State that must survive a new look (open commands, output) is
keyed by rule id. `State` also keeps the last look's findings, which are
ticked and open, the last clean's `Outcome` and `junk.json` (the reminder:
off, weekly or daily; when it last looked and what it found). The look
runs `freesize_core::junk::find` on a worker thread, as does cleaning,
which reports through a shared `Progress` (one `Step` per rule, files and
bytes so far, a stop flag checked between rules): safe rules are deleted
item by item, "check first" rules go through `quarantine::delete_group`
into one batch called "Clean junk", each rule a group, and the finding is
reported to the Hub as a `JunkReport`. The tick refreshes the progress
page while it runs and shows the results page from the `Outcome`. On a
phone the findings come from the apps report instead
(`phone_findings`, turning `cleanup::Junk` into core findings with the
phone's own rules for the trash, installers, thumbnails and leftovers),
and home shows one Clean junk card opening the same pane. The reminder's
look runs at start and on the timer when due and only changes the start
screen's card, which says how much there is to review. The Hub's
`clean_junk` request on a desktop finds the rule afresh and cleans it the
same way (`hub::AppExecutor::clean`).

`updater.rs` keeps `update.json` (`check`, off until turned on; `channel`;
`checked`; one drop-down in Settings sets both) and the state machine shown
under Updates there: `Checking`, `Found`,
`Downloading`, `Failed`. The check and the install run on worker threads
through the agent's `update` module with the app's transport; the Hub's
hint from a poll (`Event::Latest`) shows as "Your Hub says…" without the
device's own check. A desktop install swaps `freesize-gui` and `freesize`
beside the running one and relaunches through `install::relaunch`, then
quits the event loop; `updater::tidy` at start removes the `.old` file.
Android writes the verified APK to `Download/FreeSize` and opens it
through MediaStore (`android::install_apk`, retrying while Android indexes
it), which starts the package installer; it never installs on its own.

`cleanup::reviews` finds what's worth looking through but isn't junk:
screenshots in `Pictures/Screenshots` and `DCIM/Screenshots` not changed
for 30 days, and chat apps' media folders (a table of known folders per
app, old and new layouts). Their cards only have Review, which scans the
folder (after a sheet to pick one when there are several) and opens its
files list, filtered to the old screenshots where that applies.

The history also keeps a daily size sample per app (about four months) and a
daily snapshot of the storage breakdown (a year). It is written as compact
JSON.

`growth.rs` reads growth from those samples: `change` compares the newest
sample with the newest one at least 30 days older (or the oldest, with less
history), and `forecast` fits a least-squares line through the last 30 days
of used space and divides the free space by its slope. Forecasts need a
week of history and are shown only up to about two years out. The same code
gives drives their trends: `record_drives` stores each start-screen drive's
used space once a day in `drive-history.json` in FreeSize's data folder
(`%LOCALAPPDATA%\FreeSize`, `~/Library/Application Support/FreeSize`,
`$XDG_DATA_HOME/freesize` or `~/.local/share/freesize`, and the app's
private storage on Android). Samples taken when the drive had another size
are ignored, so a replaced disk starts a new history.

`appbubbles.rs` lays out an app group as bubbles sized by what the group
ranks apps by (size, cache, growth, saved files), and an app's parts when
zoomed in. `home.rs` turns all of this into the rows of each place (drives and the
breakdown bar on Home, the Free up space cards on Clean, the groups on
Apps), the rows of a group's page (a
header with the way back, then the group's apps) and the app sheet: the facts
and buttons shown when an app is tapped. A group opens as a page in place of
the list of groups, so a tap visibly does something; the controller keeps the
home screen's scroll position meanwhile and puts it back on Back.

Android's clear-cache dialog (MediaProvider's) only empties
`Android/data/*/cache` in shared storage, never the cache in apps' private
storage, and it closes without showing anything for an app without All
files access. So the card shows the shared part
(`StorageStats.getExternalCacheBytes`, Android 12+) and offers the dialog
only when that's worth it; the controller checks All files access first,
says so if the dialog doesn't appear within 3 seconds, and otherwise
compares the shared cache before and after and says what was freed.

Open hands a file to another app as its MediaStore entry
(`content://media/external/...`, from a query on `_data`) with
`FLAG_GRANT_READ_URI_PERMISSION`, because other apps can't read FreeSize's
file paths. A file MediaStore hasn't indexed yet is passed to
`MediaScannerConnection.scanFile`, and the controller retries for a few
seconds.

The actions go through intents: `ACTION_DELETE` to uninstall (needs
`REQUEST_DELETE_PACKAGES`), `market://` links for the store, the app's info
page for archiving and clearing its cache, and
`StorageManager.ACTION_CLEAR_APP_CACHE` for every app's cache folder in shared
storage. No app can clear another app's private cache. "Save APK" copies `ApplicationInfo.sourceDir`
and any split APKs to `Download/FreeSize`. While a system screen is open,
FreeSize's window loses focus; the controller polls `hasWindowFocus` and
measures the apps again when the user comes back.

The home screen is a `ScrollView` with a plain layout rather than a
`ListView`. The list view keeps a reused row's old height when its wrapped
text changes, so switching groups made rows overlap. The home screen has at
most a few hundred rows, which a plain layout handles easily.

Debug builds started with `FREESIZE_FAKE_APPS=1` (or `=no-access`) show sample
app data on the desktop, for working on the UI without a phone.

### Android packaging

`scripts/android/build-apk.sh` is the one way to build the APK, locally and
in CI. `cargo-apk` builds the Rust side for both targets from
`[package.metadata.android]` in `crates/app/Cargo.toml`; since 0.14 the APK
also holds Java classes (`HubJob`, and since 0.15.1 `PairLink`), which
`cargo-apk` can neither compile nor declare, so the script takes its APK apart and puts it together again:
the native libraries, `classes.dex` from `javac` and `d8`, the resources
compiled by `aapt2`, and the hand-written manifest
`crates/app/android/AndroidManifest.xml` (the same as the Cargo metadata,
plus the service and the notification and boot permissions, and the
pairing link filter moved to `PairLink`; keep the two in step) linked by `aapt2` with the version code `cargo-apk` gave the build,
then `zipalign` and `apksigner` with the same key. The manifest sets
`extractNativeLibs="false"` (the libraries are stored uncompressed and
mapped from the APK) and `allowBackup="false"` (the phone's pairing key
stays on the phone).

- **One universal APK** with `arm64-v8a` (phones) and `x86_64` (emulators,
  Chromebooks) code, so nobody can download the wrong file.
- **16 KB pages**: native code is linked with `-z max-page-size=16384`. Android
  15+ devices may use 16 KB memory pages, and Google Play requires it.
  `cargo-apk` replaces `.cargo/config.toml` rustflags, so the flag is passed
  through `RUSTFLAGS`, which it keeps.
- **targetSdk 36** (Android 16), minSdk 26 (Android 8.0). The launcher activity
  is `exported`, which Android 12+ requires; `cargo-apk` adds that for
  targetSdk 31+.
- **No activity restarts**: `configChanges` covers rotation, resizing, folding,
  dark mode, font size and locale. Restarting a NativeActivity would call
  `android_main` again in the same process. `launchMode="singleTask"` keeps a
  second launch from starting another instance.
- **Edge to edge**: on Android 15+ the app draws behind the status and gesture
  bars. The UI pads itself with the window's `safe-area-insets`, which Slint
  fills from Android's window insets.
- **Back gesture**: Slint delivers Back as `Key.Back`. A `FocusScope` around
  the UI steps back through dialog, scan, zoom and drive list, and only
  unhandled Back closes the app.
- **Theme and icons**: `crates/app/android/res` holds a light and dark window
  theme (matching background, readable status bar icons) and an adaptive
  launcher icon with a monochrome layer for themed icons.
  `android/make_icons.py` regenerates the icons.
- **Signing**: `apksigner` adds v2/v3 signatures. The key is
  `~/.android/disktree-release.keystore`, created on the first build and
  reused after that so updates install over each other, unless
  `CARGO_APK_RELEASE_KEYSTORE` names another one.
- **Setup**: `scripts/android/setup-linux.sh` installs the SDK, NDK, Rust
  targets and `cargo-apk` with pinned versions. `scripts/android/env.sh` finds
  the SDK and NDK for the other scripts.

`scripts/android/check-apk.sh` checks the result and fails the build if any of
these doesn't hold:

- both ABIs are present;
- minSdk is at least 26 and targetSdk at least 35;
- the launcher activity is exported and the APK isn't debuggable;
- the storage permissions are declared (All files access, Usage access, the
  installed-apps list), and since 0.14 the notification and boot ones;
- `classes.dex` is there and the manifest declares the `HubJob` service
  and the `PairLink` activity;
- there is a v2/v3 signature;
- every LOAD segment of every `.so` is aligned to 16 KB, and the zip passes
  `zipalign -c -P 16`.

Without the Android SDK, `cargo check --target aarch64-linux-android` still
type-checks the Rust side when `ANDROID_JAR` points at any `android.jar`
(Robolectric's `android-all` jar from Maven Central will do) and
`ANDROID_D8_JAR` at a d8 stand-in, which Slint's Android backend needs for
the helper class it embeds; the real APK needs the real tools.

## Hub

The Hub is three crates: what devices and the Hub say to each other
(`proto`), the device side (`agent`, used by both the app and the command
line), and the server (`hub`).

### Protocol (`crates/proto`)

- **Keys** (`keys.rs`): Ed25519 key pairs. A key's fingerprint is the first
  10 bytes of its SHA-256 in Crockford base32, `XXXX-XXXX-XXXX-XXXX`, short
  enough to compare by eye or type.
- **Signed releases** (`release.rs`): `release.sh` signs `SHA256SUMS.txt`
  with the release key (domain `freesize-release-v1`, so a release
  signature can't stand in for a device's); the public half is compiled in
  from `release/public-key.txt` (`release_key()` is `None` while the file
  holds no key), and `matches_sums` checks a downloaded file against the
  signed sums. `PollResponse.latest` is the Hub's hint about a newer
  release, only ever a pointer: the device verifies before installing.
- **Signed requests** (`signing.rs`): every call a device makes carries
  `x-disktree-device`, `-time`, `-nonce` and `-signature` headers. The
  signature covers `disktree-request-v1`, the method, path, time, nonce and
  the SHA-256 of the body. The Hub refuses a time more than 5 minutes off
  and any nonce it has seen.
- **Signed messages** (`Signed<T>`): the JSON text, the signer's
  fingerprint and a signature over a purpose prefix
  (`disktree-command-v1`, `-decision-v1`, …) and the text, so a signature
  made for one kind of message never passes as another. Commands are signed
  by the Hub, decisions by the approver's device.
- **Pairing links** (`pairing.rs`):
  `freesize://pair?hub=<url>&code=XXXX-XXXX-XXXX&key=<fingerprint>`;
  `disktree://` links from before the rename are read too. Codes
  are 60 random bits in Crockford base32, which forgives O/0 and I/1 mix-ups;
  the Hub stores only their hash, for 10 minutes and one use, and allows 10
  wrong guesses a minute from one address and 20
  wrong guesses a minute.
- **Messages** (`messages.rs`): `DeviceInfo` (name, platform, what it can
  do, whether someone is at it, its approver and grants, and `rights`: whose
  eyes its scans see with), `Report` (info, disks, a scan summary with the
  count and the first paths of folders it couldn't read, Android's apps,
  the junk cleaner's findings as a `JunkReport`: per rule its id,
  category, size, items, safety and whether it refills or is running), `PollResponse` (commands, approval
  requests for another device, decisions, withdrawals), `Command` with its
  `Action` (`delete`, `uninstall`, `clean_junk`, `restore`,
  `empty_quarantine`, `docker_cleanup`) and `CommandUpdate`. `unraid.rs`
  holds what an Unraid server reports (the array, shares, Docker, VMs,
  activity, permissions, moves with the file being copied, Nextcloud's
  data folders and the round's peak memory). `sync.rs` (0.12) is what a
  device keeps of its Nextcloud folders: per account (server, user,
  client) and sync folder (local path, folder on the server, size, bytes
  stored on the device, file counts, the virtual-files mode) its
  subfolders two levels down and files of 100 MB and over one by one, each
  with whether it is stored on the device. Each action
  has an `ActionKind`, which is what a grant covers; emptying the
  quarantine and removing images are kinds of their own that no grant
  covers, so they are asked for every time.

A scan summary holds the 10,000 biggest folders (path, size, size on disk),
the 500 biggest files and the 100 biggest file types: under a megabyte for a
whole array, rather than every file.

### Device side (`crates/agent`)

- `review.rs` (feature `review`, which the app and the command line turn
  on): `words_in`, which of a list of words a file contains, for a folder
  review. Plain text as UTF-8 (HTML and XML without their tags), Office and
  OpenDocument files by unzipping their XML parts (64 MB at most),
  PDFs through `pdf-extract`, inside `catch_unwind` because it panics on
  some broken files. A word counts where it starts a word. It only returns
  the words, so nothing of the text can be sent on.

- `state.rs`: the pairing, saved as JSON with mode 0600: Hub address, name
  and key, this device's id and private key, the approver's device and key,
  grants, and the last 500 handled commands (so a redelivered one never runs
  twice).
- `transport.rs`: HTTP through a `Transport` trait; every request says what
  to trust (`Trust::System`, or `Trust::Authority` with the Hub's own
  authority as PEM, used for the Hub's address and nothing else). Desktops
  and servers use ureq (the system's TLS on Windows and macOS, rustls with
  the system's certificates elsewhere, no redirects), with exactly the
  authority as root when one is pinned; a separate `download` method
  follows redirects with the system's certificates, for releases on
  GitHub (up to 512 MB); Android passes Java's
  `HttpURLConnection` in through JNI, with an `SSLSocketFactory` whose trust
  manager holds only the authority when one is pinned, and when Java
  throws, the error is Java's own message (the manifest allows plain
  `http://` for Hubs with TLS off).
- `client.rs`: `hello` (the Hub's name, key and authority), `pair`
  (refuses a Hub whose key doesn't match the link's fingerprint, or asks the
  person to compare it when the link has none; the first hello is over
  https with the system's certificates or, failing that, plain http on the
  same port, which a Hub with its own authority answers naming it; pairing
  then goes over https trusting that authority, and the signed answer must
  name the same one), `move_to` (a signed `HubAddress` from a poll: only
  the Hub's own signature counts, and the new address must answer with the
  Hub's key before the state changes), and a signed `Client` for reports,
  polls, updates and decisions.
- `commands.rs` decides what to do with a command:
  1. it must be signed by the pinned Hub key, for this device, not expired
     and not handled before;
  2. a delete must stay inside a folder this device scans, with no `..` and
     no symbolic link leading out (parents are resolved, and the folder
     itself must resolve to somewhere inside a scanned folder, so a scanned
     folder that is a link opens nothing), and never touch protected system
     paths or FreeSize's own files;
  3. a grant for that admin and kind of action runs it without asking;
     emptying the quarantine and removing Docker images never run under a
     grant, and "approve for N minutes" on them means once;
  4. otherwise a device with someone at it asks them; one without asks its
     approver, and one without an approver refuses.

  An approver's decision must be signed by the approver's pinned key and
  name the hash of the exact command; grants last at most 8 hours. A
  `Policy` can carry extra `Guard`s that refuse paths on their own terms
  (Unraid's rules, open files).
- `quarantine.rs`: on servers a delete is a move into `.disktree-quarantine`
  at the top of the file system the item is on: a rename, never a copy, in a
  batch with a manifest of where each item was. A batch is put back (never
  over something that's there now) or emptied only on request.
- `estimate.rs`: what deleting would really free: hard-linked files count
  only when every link goes, and ZFS snapshots holding the data are named.
- `guard.rs`: refuses files some process has open (`/proc/*/fd`).
- `nextcloud.rs` (0.12): the desktop client's accounts and sync folders
  from its `nextcloud.cfg` (`%APPDATA%\Nextcloud`, `~/Library/Preferences/
  Nextcloud` or `~/.config/Nextcloud`, this user's and every user's when
  the agent runs with the rights to read them; on macOS with virtual files,
  `~/Library/CloudStorage/Nextcloud-*`), the Android app's folder under
  `Android/media/com.nextcloud.client/nextcloud/<user>@<host>`, and
  `measure`: a scan of each sync folder summed per subfolder, with the
  biggest files one by one. A file is stored on the device when its size is
  zero or it has blocks allocated (Windows marks placeholders with the
  cloud attributes, and `platform.rs` gives those no allocation; macOS's
  dataless files have none); in the client's "suffix" mode, `x.nextcloud`
  is a placeholder for `x`. The client's sync journal (SQLite, kept open by
  the client) isn't read: paths are the key, the same on every client of a
  server, and the Hub matches them to the server's file ids itself.
  Since 0.13 it also carries the actions: `online_only` marks files below
  a sync folder with the cloud files API's pin attributes on Windows
  (`FILE_ATTRIBUTE_UNPINNED` to free, `PINNED` to keep; the Nextcloud
  client dehydrates or hydrates them on its next run, and only what it
  has uploaded), and on other platforms answers with where the client's
  own menu is; `move_in` moves a scanned folder into a sync folder, a
  rename on the same device, else a copy, a byte-for-byte check of every
  file, and the original into the quarantine, with a failed check
  removing the copy instead.

The command line's agent (`crates/cli/src/agent.rs`) reports disks hourly,
scans its folders on a thread every `--every` hours, and long-polls the Hub
in between. The app (`crates/app/src/hub.rs`) has a worker thread for
reports and answers and a second one for polling, so an answer goes out at
once; the first poll after starting asks the Hub to send everything still
open again, so a prompt shown before the app closed comes back. Prompts go
to the controller through a channel; deletes go to the app's quarantine
as one batch under the request's id, and the executor's note tells the
approver so. The command line's agent sends deletes to the quarantine too.
- `update.rs`: `check` asks GitHub's releases API (a `User-Agent` naming
  the version, nothing else) for the newest release on the channel newer
  than this build, skipping drafts and, on the stable channel,
  pre-releases; `verified_sums` fetches `SHA256SUMS.txt` and its `.sig`
  and refuses anything the built-in release key didn't sign (or everything,
  in a build without the key); `download` fetches the file for this
  platform (`asset_name`: the Linux tarball, the Windows zip, the APK) and
  refuses one that doesn't match the signed sums. `install.rs` (feature
  `install`, desktops) unpacks `.tar.gz` and `.zip`, writes the new file
  beside the old, renames the old to `.old` and the new into place (Windows
  can rename a running program but not overwrite it), and relaunches.

### Server (`crates/hub`)

Axum on Tokio, one SQLite database (rusqlite, bundled) behind a mutex,
queried on Tokio's blocking threads. `db.rs` applies numbered migrations
once each (`PRAGMA user_version`): devices, pairing codes, nonces, disks
and their daily samples, scans (folders as gzipped JSON) and their files,
apps, commands, approvals, the audit log, sessions and sign-ins; then
devices without an agent and saved searches; then alerts and their
channels; then what Unraid servers report and the quarantine; then the
folders a scan couldn't read; then each device's junk report; then
Nextcloud accounts, their listings and each device's sync report. The dashboard marks such a scan incomplete,
with the count, on the device card, the scans table and the folders page
(which names them), as does the HTML report; a Windows agent's page says
it scans as its user.

- `auth.rs`: OpenID Connect with PKCE (S256), state and nonce. The ID token
  is checked against the provider's keys (JWKS, or HS256 with the client
  secret), issuer, audience and expiry. Roles come from the `groups` claim.
  Sessions are a random token in an HttpOnly, SameSite=Lax cookie (Secure
  over HTTPS), stored hashed; changes need the session's CSRF token in a
  header.
- `tls.rs`: HTTPS. With `--tls auto` (the default) the Hub makes its own
  authority on first start (`tls-ca.pem`, key 0600, ECDSA P-256, 20 years,
  CA with path length 0) and a certificate for the host of its address
  (`tls-cert.pem`: SHA-256, the host as DNS or IP subject alternative name,
  serverAuth, 825 days, Apple's limit), renewed a month before it ends or
  when the host changes; `--tls-cert-file`/`--tls-key-file` load an
  administrator's PEM instead and reload it when the files' times change;
  `--tls off` is plain http only. `lib.rs`'s accept loop (hyper-util, not
  `axum::serve`) peeks the first byte of each connection: 0x16 is a TLS
  handshake (rustls with ring, ALPN h2 and http/1.1), anything else plain
  http on the same port, and a `Via { tls, peer }` extension says which.
  `web::https_only` sends plain-http requests for the dashboard to
  `https://` on the same host and port, leaving `/api/v1/` (devices from
  before 0.8.7) and `/api/status` (the health check) alone; `/ca.pem` hands
  the authority out. With TLS on, `Config::public_url` is `https://`
  whatever was given, so pairing links and the sign-in redirect URI are.
- `devices.rs`: the device API under `/api/v1/` (`hello`, `pair`, `report`,
  `poll`, the approver's key by fingerprint) behind the request signature
  check. `hello` names the Hub's own authority; a poll over plain http from
  a Hub that serves HTTPS answers at once with `move_to`, the public address
  and authority signed by the Hub. A poll waits up to 60 s on a per-device
  `Notify`, woken when something arrives for it.
- `commands.rs`: requests from the dashboard, checked against what the
  device reported (the folder must be one the device scanned, and the sizes
  shown come from that scan; an Unraid finding, app, junk kind (from the
  apps report or the junk report, never a rule that needs the system's
  tool or whose program is running) or
  quarantine batch must be in its report; from 0.8.4 the paths of a delete
  must be in the scan too), signed and queued. The device checks every
  path again before asking anyone. States: queued, delivered,
  shown, needs_approver, running, done, failed, denied, expired, cancelled.
  Undelivered work is sent again after 10 minutes and expires after a day.
  For a device without anyone at it, the request goes to the approver's
  device and the signed decision back. The 0.13 actions: `online_only`
  needs a Windows device whose sync report names the folder with virtual
  files on (`wincfapi`), paths among its reported folders and big files,
  and takes the bytes from the report; `move_in` needs the folder in the
  device's newest scan and the sync folder in its report, with the target
  cleaned to plain names, and once queued the Hub makes the folder on
  Nextcloud (`MKCOL`, each level) when it is connected to that account;
  `external_storage` needs the Unraid report to name the container, the
  share and the share's path inside the container, and refuses a mount
  point Nextcloud already has.
- `dashboard.rs`, `reports.rs`: devices, scans, folders, groups and pairing
  codes; comparing scans (`snapshot::compare` from core), search across the
  newest scans, CSV/JSON exports, the HTML report (its own strict CSP) and
  uploads of ncdu files for devices without an agent, which get a key
  nobody holds.
- `history.rs`: keeps every scan for 7 days, then one a day to 90 days, one
  a week to a year and one every 30 days after that.
- `alerts.rs`: every minute, works out what's wrong (full disks, with a
  point of hysteresis; a week's growth; a least-squares forecast over 30
  days of samples; quiet servers; waiting requests), opens and closes alerts
  and sends what changed to ntfy (JSON publishing), Gotify, Pushover or a
  webhook signed with HMAC-SHA256.
- `metrics.rs`: Prometheus text and the same as JSON, behind a bearer token
  compared in constant time.
- `audit.rs`: an append-only log; each entry's hash covers its fields and the
  previous entry's hash, and `verify` finds the first changed or missing
  one. `head` is the newest entry's id and hash: `maintenance.rs` logs it
  once a day and `alerts.rs` appends it to every alert's text, so the
  chain's end is recorded outside the database.
- `limits.rs`: attempts in the last minute per address and in all, for
  wrong pairing codes (10 and 20) and sign-in starts and failed callbacks
  (30 and 300); the address comes from the connection (`Via`).
- `maintenance.rs`: once a day, scans and disk samples older than
  `--retention-days` go (0 keeps them, thinned by `history.rs`), and the
  audit head is logged.
- `updates.rs`: with `FREESIZE_UPDATE_CHECK=on`, once a day the agent's
  `update::check` asks GitHub for the newest release on the channel; the
  result is in `Shared.latest`, `/api/me` shows it in Settings, and `poll`
  answers with `latest` to a device whose reported version is older. The
  Hub never downloads or installs anything.
- `dashboard.rs` also reads each device's app version out of its name and
  marks it `behind` when it's older than the Hub's, which the device cards
  and the device page show.
- `nextcloud.rs` (0.12): the Hub's own Nextcloud account(s). `connect`
  starts Login Flow v2 (`POST <server>/index.php/login/v2`), keeps the
  flow in `Shared.nextcloud` and polls its endpoint every 5 s for up to 20
  minutes on a task until Nextcloud answers with the server, the login
  name and an app password, which go in the `nextcloud` table (the
  password is never sent to the dashboard); the dashboard asks
  `/api/nextcloud/connect/{flow}` meanwhile. `list_files` lists the
  account over WebDAV: PROPFIND with Depth 1 (ureq with non-standard
  methods allowed) on `remote.php/dav/files/<user>/`, every folder two
  levels down and deeper where a folder holds 100 MB or more, at most
  5,000 requests, reading `oc:fileid`, `oc:size`, `oc:checksums`,
  `d:getlastmodified` and the root's `d:quota-used-bytes`, plus the
  trash's total from `remote.php/dav/trashbin/<user>/trash`; the
  multistatus XML is read by a small tokenizer that goes by local names,
  whatever prefixes the server uses. Listings run at start, every six
  hours and on request, one per account at a time, replacing the
  `nextcloud_files` rows in one transaction. Disconnecting asks Nextcloud
  to revoke the app password (`DELETE ocs/v2.php/core/apppassword`) and
  drops the rows. The Hub reads only, apart from 0.13's `mkcol`, and
  since 0.13 reads the root's `d:quota-available-bytes` (`quota_free`, -1
  when unlimited) for the move-in suggestions.
- `copies.rs` (0.12): `join` puts the devices' sync reports (the `sync`
  table) together with the listings on (server, user, path): per folder
  two levels down, what the server holds and each device's stored bytes,
  file counts and whether it is complete; per big file, the server's id
  and checksums, whether it is on the server and which devices store it;
  and the risks: a big file or folder one device alone holds that the
  server lacks (only for accounts the Hub has listed), top folders only
  the server holds, and the biggest depth-one folders of each device's
  latest scans (1 GB and over, outside system folders) that no synced
  folder covers. `GET /api/copies` runs it for the devices the person may
  see. `suggestions` (0.13) turns the join and the devices' other reports
  into requests the dashboard sends as they stand: free a folder's local
  copies (the server has it whole, and another device keeps it whole or
  the disk is under 15% free; an `online_only` action on Windows with
  virtual files, a pointer to the client elsewhere), camera files under a
  phone's `DCIM` matched to the listing by name and size (a `delete` to
  the phone's quarantine, the 500 biggest), the not-synced folders of a
  device that syncs (a `move_in` to `Archive/<device>/<folder>`, skipped
  when the account's free quota is known and too small), and shares an
  Unraid Nextcloud container mounts that aren't external storage yet.
- `web/`: the dashboard, plain JavaScript without a build step. All text
  goes in through `textContent`; the page's CSP allows scripts and styles
  from the Hub only, so styles are set through the CSSOM.

The tests in `crates/hub/tests` run a Hub on a random port against a mock
OpenID provider (`tests/common/idp.rs`) and pair real agents with it:
sign-in, pairing, signatures and replays, scoped viewers, requests and
approvals through an approver, comparing, search, exports, uploads, alerts
to a local ntfy and webhook stand-in, metrics, and connecting a stand-in
Nextcloud (the sign-in flow, PROPFIND listings, the Copies join, the
risk report, the suggestions and their requests, with MKCOL for a move).
`examples/demo.rs` runs
the same with made-up devices for trying the dashboard.

### Packaging

- `Dockerfile`: builds on the build machine's architecture and
  cross-compiles for amd64 or arm64, so a multi-architecture image needs no
  emulation. The image is Debian slim with the Hub and an entrypoint
  (`packaging/docker/entrypoint.sh`) that gives `/data` to `PUID:PGID` and
  drops to that user with `setpriv`. `freesize-hub health` is the health
  check.
- `unraid/freesize-hub.xml`: the Unraid template.
- `unraid/plugin/`: the Unraid plugin (below).
- `packaging/systemd/`: units for the Hub (its own dynamic user and
  hardening) and the agent, with their settings files.

## Unraid

`unraid/freesize-hub.xml` is the Hub's Docker template and `unraid/ca/`
holds what Community Applications reads besides it: `ca_profile.xml` (the
maintainer) and `freesize.xml` (the plugin's entry, pointing at the
release's `.plg`). CA scans the whole repository for XML, so the Android
resource files show up as non-templates in its statistics and nothing
else; the owner submits the repository through CA's form.

The plugin is the command line, built statically with musl so it runs on
any Unraid version, plus a page in Unraid's web UI. `crates/unraid` knows
the server; every module reads Unraid's own files through `System`, the
server's root (`/`, or a folder laid out like a server in the tests).

- `array.rs`: disks and pools from `/var/local/emhttp/disks.ini` and
  `/proc/mounts` (kind, device, file system, size, free space, temperature,
  whether it's spun down), and whether the array is started (`var.ini`).
  Maps a `/mnt/user` path to the disks that hold it.
- `shares.rs`: share settings (`shares.ini`) and each share's size per
  disk, from this round's scans or, for a disk that slept through it, the
  last one (kept in `disks.json` on the flash drive).
- `docker.rs`: Docker's settings (`docker.cfg`), the Engine API over its
  Unix socket (a small HTTP/1.1 client: disk usage, containers, inspect,
  image removal, build cache prune), dockerMan's templates, and findings
  sorted into safe (dangling images, build cache), check first (unused
  images, orphan volumes, appdata no installed app uses) and in use. Logs
  and writable layers are only reported.
- `vms.rs`: `domain.cfg` and libvirt's XML: each VM's disks, CD drives and
  snapshots; ISOs no VM uses, and files in a VM's folder not attached to it.
- `protect.rs`: what's never offered (flash, system share, Docker's
  storage, installed apps' appdata, container mounts, VM disks and ISOs in
  use, whole disks and shares, remote mounts, the quarantine). `whole`
  rules (a disk, a share, a share's folder mounted into a container) keep
  the path itself from being deleted but let what's inside move between
  disks. `UnraidGuard` applies the rules to every Hub request;
  `UnraidPlaces` gives each disk (or ZFS dataset) its own quarantine.
- `activity.rs`: a sample a minute of each disk's sleep state and
  `/proc/diskstats`, for spin-ups, awake time, reads and writes over the
  last day; open files from `/proc/*/fd`, put on disks and named by
  process and container (from its cgroup).
- `perms.rs`: files in user shares not owned by nobody:users or without
  group read and write (app, system and VM shares are skipped).
- `mover.rs`: a plan (room with a margin of 1 GiB or 1%, files in the way,
  hard links that would be split, the in-use rules), then file by file:
  copy to a hidden `.name.disktree-part`, compare the xxh3-128 hashes of
  both, keep owner, mode, times and extended attributes (`llistxattr`,
  `lgetxattr`, `lsetxattr`), rename into place. A sparse original (less
  on disk than its length) is written with its holes: 64 KB runs of zeros
  are seeked over and the length set at the end. Partial copies an
  interrupted move left on the target are removed first. The job carries
  the file being copied (`current`). The originals go to the source
  disk's quarantine, or, with `empty` ticked when the move was planned,
  are deleted once every copy checked out. Cancelling stops between
  files. Also the folders in a share's folder per disk, for the page's
  picker (sleeping disks only when asked), the files that are in more
  than one of several source disks, which stop a move from all of them,
  and the presets: `empty_disk` places each share on the disk with its
  files on the roomiest disk the share's include and exclude settings
  allow (a margin of 1 GiB or 1% kept, earlier steps' bytes counted),
  folder by folder when the whole share fits nowhere; `gather_share`
  moves a share's parts on awake disks to the disk holding most of it or
  the one named. Both are plans of `Step`s the daemon runs as ordinary
  queued moves.
- `scans.rs`: this round's scans, to answer questions about any server
  path without touching the disks again. A disk's tree is dropped as soon
  as it's scanned: `DiskScan::compact` keeps an index (a `BTreeMap` of
  relative paths) of every folder, plus the files under the places later
  steps look at file by file (`Keep`: ISOs, VM folders and disk images,
  the direct entries of appdata, Nextcloud's versions and trash, with `*`
  for one name), so a big array's files never sit in memory at once. The
  round reports its peak memory (`VmHWM`) and the daemon logs it.
- `nextcloud.rs`: Nextcloud's data folders, found by `.ncdata` or
  `.ocdata` down to three levels on each awake disk (a share, or
  `appdata/<app>/data`), with `config.php` beside them or in the same
  appdata folder. The report sizes each user's `files`, `files_versions`,
  `files_trashbin`, `uploads` and `cache`, `appdata_*/preview` and the
  rest of `appdata_*`, and group folders, from the scans; since 0.13
  `container_for` names the container binding the data folder (or one
  above it; Nextcloud's image, then a running one, then the nearest
  bind), `share_mounts` the shares it binds with their paths inside,
  `external_storages` what `occ files_external:list` answers (run with
  `docker exec`, as `www-data` with `php occ`, else the image's own `occ`;
  only on the server itself), and `add_external_storage` runs
  `files_external:create <mount> local null::null -c datadir=<inside>`
  for an approved request, after the daemon has checked the bind against
  Docker's inventory; the retention
  settings (`versions_retention_obligation`, `trashbin_retention_obligation`,
  `auto` by default) give a latest day where they name one, and the
  versions and trash older than it are counted as expired. Nothing is
  offered for deletion.
- `round.rs`: one look at the server: scan the disks that are awake (and
  one that slept for `WAKE_DAYS`), then shares, Docker, VMs, Nextcloud,
  permissions and the rules.
- The junk cleaner's Unraid rules (`freesize_core::junk`, `$SHARES` as
  `/mnt/user`): the Recycle Bin plugin's `.Recycle.bin` folders and the
  `.Trash-*` folders clients leave. `freesize unraid junk list|clean` and
  the page's Quarantine tab ("Trash folders in the shares", with every
  path and an Empty for good button) use them, and the hourly report to
  the Hub carries them.

The daemon (`crates/cli/src/unraid.rs`, `freesize unraid daemon`) runs a
round every `SCAN_HOURS` on a thread (and as soon as a stopped array
starts), samples activity each minute, runs moves on a thread of their
own, one at a time (a move from several disks queues one per disk, each
planned again just before it starts), and writes `status.json` to `/var/local/freesize` for the page. The page
asks it for things with files in `requests/` (scan, refresh, settings,
hub, `move-<id>.json`, `cancel-<id>`); quick actions (quarantine,
estimates, plans, Docker cleanup) it runs with the same command directly.
Once paired (`agent.json` on the flash drive), a second thread long-polls
the Hub, and the daemon carries out what the approver allows through
`UnraidExecutor`: deletes go to the quarantine, and putting back, emptying
and Docker cleanup work as on the page.

The page (`unraid/plugin/root/usr/local/emhttp/plugins/freesize`):
`FreeSize*.page` are Unraid's tabs, in a section of Tools of their own
(`DiskSpace.page`). `freesize.js` draws them from `include/status.php` and
posts to `include/action.php`, which checks each value and runs
`freesize` with every argument quoted. Unraid's own prelude
(`local_prepend.php`) checks the CSRF token of every POST and removes it
before `action.php` runs; `action.php` checks the token itself only when
that prelude didn't run. Pairing attempts go to `/var/log/freesize.log`.
On Unraid, `freesize agent` and `freesize approver` default to the
plugin's `agent.json`, and pairing there asks the daemon to report. Until a round
has finished (`last_round` empty) the page says it's still looking, with
the daemon's `doing` ("scanning disk2 (2 of 5)"), rather than reading a
missing Docker or VM section as switched off. Unraid's stylesheet keeps
every table cell on one line with a fixed height; `freesize.css` undoes
that for its own tables, and lists findings in grid rows (`.dt-rows`), so
long paths wrap and the buttons stay on the page. `rc.freesize`
starts the daemon at nice 19 with idle I/O priority; the array's events
(`event/stopping`, `stopped`, `started`) stop it before the disks unmount
and start it again. `scripts/unraid-plugin.sh` packs the static binary
and these into a Slackware `.txz`, and fills in the `.plg`'s version and
the package's SHA-256. Until 0.16.2 releases also carried `disktree.plg`
(and the Hub's image under its old name, `disktree-hub`) to move installs
from before the rename over; they stopped at the owner's word, and
servers still on DiskTree install `freesize.plg` by hand.
`freesize.plg`'s first script still copies `/boot/config/plugins/disktree` (with
`disktree.cfg` renamed), stops and removes the old package, its page and
its `.plg`.

## Testing

- `cargo test --workspace` runs the core tests (sizes, contiguity, hard links,
  symlinks, permissions, cancellation, deletion, squarify geometry), the
  app's helper tests, the protocol and agent tests (signatures, pairing
  links, which commands a device runs, refuses or asks about), the Unraid
  tests (each against a made-up server folder: disks, shares, Docker over
  a fake socket, VMs, protection, quarantine, moves), and the Hub's
  end-to-end tests.
- `php unraid/tests/page_test.php` runs the plugin page's `action.php` as
  Unraid does, behind a copy of Unraid's token check and with a stand-in
  `freesize` command: pairing, saving, a wrong token, a refused pairing.
- `scripts/release.sh` runs them before building a release on the build
  machine. The CI workflow runs them on Windows, macOS and Linux and builds
  the APK, but only when started by hand.

## Releases

`scripts/release.sh` builds a release on a Linux machine and publishes it
with the GitHub CLI: the APK through `build-apk.sh` (so it is checked and
signed with the machine's key), the Linux app natively, and the Windows app
cross-compiled for `x86_64-pc-windows-gnu` with MinGW, plus the Hub for
Linux (and the Hub and command line for ARM when the aarch64
cross-compiler is there) and the Unraid plugin (the command line for
`x86_64-unknown-linux-musl`, packed by `scripts/unraid-plugin.sh`). The Windows executables only import system DLLs,
so they need no MinGW runtime. It also builds the Hub's Docker image for
amd64 and arm64 and pushes it to GitHub's registry, logged in with the
GitHub CLI's token in a throwaway Docker config. The image is the
Dockerfile's final stage with its `hub` stage replaced by the Hubs the
release built (a named build context), so Docker compiles nothing: the
build machine's big disk is a 9p share Docker can't keep layers on, and
the root disk has little room. Those Hubs link to the build machine's
glibc, so the script checks they need no newer one than the image's Debian
has. Every release, drafts too, is tagged `<version>` and `latest`, since
drafts are often made public on GitHub's website, which can't move the
tag; `--publish` makes a draft public and points `latest` at it again. The script only publishes a clean checkout that
matches GitHub, and lets `gh release create --target` create the tag for
that commit.

`SHA256SUMS.txt` is signed with the release key when
`~/.freesize/release-key` (or `FREESIZE_RELEASE_KEY`) exists, with
`freesize release sign` and checked back with `freesize release verify`
against `release/public-key.txt`; without the key the script warns and the
release is unsigned, so apps won't install it themselves. `freesize release
key` makes the key once. `TESTING.md` is the by-hand checklist for a
release.

## Possible next steps

See [docs/ROADMAP.md](docs/ROADMAP.md): what's left of the Hub and the
Unraid plugin, and Nextcloud integration. The Windows MFT reader (reading the NTFS Master File Table
directly, as WizTree does, for a whole-drive scan in seconds) would be an
optional fast path for admins.

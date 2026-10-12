# Architecture

DiskTree is a Cargo workspace with three crates:

```
crates/core   disktree-core   scanner, tree model, stats, treemap and bubble
                              layout, snapshots, scan files
crates/cli    disktree-cli    command-line front end
crates/app    disktree-app    Slint UI for desktop and Android
```

All the logic lives in `core`, which has no UI or platform UI dependencies, so
it can be tested on its own. The CLI and the app are thin layers on top of it.

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
  scan. Only an unreadable root fails it.

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

### Snapshots (`snapshot.rs`)

A `Snapshot` is the folder sizes of one scan: paths relative to the scanned
folder, logical and allocated size, for the biggest folders (10,000 in the
app). Keeping folders rather than files keeps a snapshot of a whole drive
to about 1 MB of JSON, however many files it holds. `compare` lines two snapshots up by path and
reports folders that grew, shrank, appeared or went away by at least a
minimum. A folder is skipped when one of its children accounts for 80% of
its change, so the list names where the change happened rather than every
folder above it. `find` maps a snapshot path back to a node.

### Scan files (`ncdu.rs`)

`export` writes a tree in ncdu's JSON export format (version 1.2): a
directory is an array whose first element describes it, a file an object,
sizes as `asize`/`dsize` with a directory's own size separate from its
contents. Read errors, other file systems, hard links and non-regular files
map to ncdu's `read_error`, `excluded`, `hlnkc` and `notreg`. `import`
reads files from DiskTree, ncdu 1.x and ncdu 2.x's JSON export and builds the
tree the way the scanner does (breadth first, so children stay contiguous),
then aggregates the totals.

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
similar photo searches, basket jobs) runs on a plain thread and writes its
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
a snapshot and records it (`scans.rs`, the last 12 per scanned folder in
`scans/<hash>.json` in the data folder, returning the earlier ones), and,
when the scan covers a whole drive, asks the system about hidden space. A
path ending in `.json` is read as a scan file instead (`imported`), which
turns off Open, Delete and Collect.

### Scan view extras

- `changes.rs` turns `snapshot::compare` into the Changes tab's rows. The
  bubble view looks up each folder in the compared snapshot and adds the
  difference to its label when it's at least 5% (and 10 MB).
- `hidden.rs` explains used space that no file accounts for, only for a
  scan of a whole drive (`is_drive_root`: a different device than the
  parent on Unix, `C:\` on Windows). It asks the system's tools and stays
  quiet when one is missing: `zfs list` for the snapshots of the dataset
  mounted there, `vssadmin list shadowstorage` for shadow copies, `tmutil
  listlocalsnapshots` for Time Machine. `filecleanup.rs` shows the gap only
  when it's at least 1 GB and 2% of the used space.
- `moves.rs` moves basket items: the target keeps the path below the
  scanned folder, `rename` within a drive, else copy, compare sizes and
  only then remove the original. An existing target stops that item. The
  `(from, to)` pairs go to `last-move.json` so Put back can reverse them.
- `report.rs` builds the HTML report (self-contained, light and dark) and
  the CSV; `actions::save_export` asks where on desktops (`rfd`) and writes
  to `Download/DiskTree` on Android.
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

Storage volumes come from `/storage`: `emulated/0` is internal storage, and the
`XXXX-XXXX` entries are SD cards and USB drives.

Errors and panics go to logcat under the tag `DiskTree`, since stderr goes
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
opens DiskTree's own app info page.

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
JSON in `Documents/DiskTree/` (survives uninstalling DiskTree) and in the
app's private storage (works without All files access); both copies are read
and merged.

`cleanup.rs` finds space that can go without touching any app, under the
shared storage root: old `.apk` files in Download (except DiskTree's own
`Download/DiskTree`), `DCIM/.thumbnails`, and `Android/media/<package>`
folders of packages that aren't installed (only when the installed list looks
complete). Android's trash comes from MediaStore's Files collection queried
with `QUERY_ARG_MATCH_TRASHED`. The same collection's `owner_package_name`
column says which app saved each file; `media_by_owner` adds those up per app
and picks the folder holding most of them for the Browse button. Deleting
runs on a worker thread after the shared confirmation dialog.

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
used space once a day in `drive-history.json` in DiskTree's data folder
(`%LOCALAPPDATA%\DiskTree`, `~/Library/Application Support/DiskTree`,
`$XDG_DATA_HOME/disktree` or `~/.local/share/disktree`, and the app's
private storage on Android). Samples taken when the drive had another size
are ignored, so a replaced disk starts a new history.

`appbubbles.rs` lays out an app group as bubbles sized by what the group
ranks apps by (size, cache, growth, saved files), and an app's parts when
zoomed in. `home.rs` turns all of this into the home screen's rows (drives, the breakdown
bar, the Free up space cards, the groups, then the selected group's apps) and
into the app sheet: the facts and buttons shown when an app is tapped.

Android's clear-cache dialog decides itself how much to clear, so the
controller notes the total cache before opening it and compares after the
user comes back; the card then says what was freed.

Open hands a file to another app as its MediaStore entry
(`content://media/external/...`, from a query on `_data`) with
`FLAG_GRANT_READ_URI_PERMISSION`, because other apps can't read DiskTree's
file paths. A file MediaStore hasn't indexed yet is passed to
`MediaScannerConnection.scanFile`, and the controller retries for a few
seconds.

The actions go through intents: `ACTION_DELETE` to uninstall (needs
`REQUEST_DELETE_PACKAGES`), `market://` links for the store, the app's info
page for archiving and clearing its cache, and
`StorageManager.ACTION_CLEAR_APP_CACHE` for every app's cache. No app can clear
another app's cache directly. "Save APK" copies `ApplicationInfo.sourceDir`
and any split APKs to `Download/DiskTree`. While a system screen is open,
DiskTree's window loses focus; the controller polls `hasWindowFocus` and
measures the apps again when the user comes back.

The home screen is a `ScrollView` with a plain layout rather than a
`ListView`. The list view keeps a reused row's old height when its wrapped
text changes, so switching groups made rows overlap. The home screen has at
most a few hundred rows, which a plain layout handles easily.

Debug builds started with `DISKTREE_FAKE_APPS=1` (or `=no-access`) show sample
app data on the desktop, for working on the UI without a phone.

### Android packaging

The manifest is generated by `cargo-apk` from `[package.metadata.android]` in
`crates/app/Cargo.toml`. `scripts/android/build-apk.sh` is the one way to build
it, locally and in CI:

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
  installed-apps list);
- there is a v2/v3 signature;
- every LOAD segment of every `.so` is aligned to 16 KB, and the zip passes
  `zipalign -c -P 16`.

## Testing

- `cargo test --workspace` runs the core tests (sizes, contiguity, hard links,
  symlinks, permissions, cancellation, deletion, squarify geometry) and the
  app's helper tests.
- `scripts/release.sh` runs them before building a release on the build
  machine. The CI workflow runs them on Windows, macOS and Linux and builds
  the APK, but only when started by hand.

## Releases

`scripts/release.sh` builds a release on a Linux machine and publishes it
with the GitHub CLI: the APK through `build-apk.sh` (so it is checked and
signed with the machine's key), the Linux app natively, and the Windows app
cross-compiled for `x86_64-pc-windows-gnu` with MinGW. The Windows
executables only import system DLLs, so they need no MinGW runtime. The
script only publishes a clean checkout that matches GitHub, and lets
`gh release create --target` create the tag for that commit.

## Possible next steps

See [docs/ROADMAP.md](docs/ROADMAP.md): the DiskTree Hub dashboard with
device-confirmed actions, an Unraid plugin, and Nextcloud integration. The Windows MFT reader (reading the NTFS Master File Table
directly, as WizTree does, for a whole-drive scan in seconds) would be an
optional fast path for admins.

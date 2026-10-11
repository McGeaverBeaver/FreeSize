# Architecture

DiskTree is a Cargo workspace with three crates:

```
crates/core   disktree-core   scanner, tree model, stats, treemap layout
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

- `largest_files` keeps a size-`n` min-heap while walking a subtree.
- `extension_stats` groups by lower-cased extension. Dotfiles like `.bashrc`
  count as having no extension.
- `treemap::squarify` is the squarified treemap algorithm (Bruls, Huizing &
  van Wijk). It keeps each rectangle's area proportional to its size while
  keeping the rectangles close to square.
- `treemap::layout` nests it a few levels deep. Each directory gets a header
  strip for its label, tiny rectangles are dropped, and the total is capped
  (1500 by default) to keep rendering cheap. Items come out parent-first, so
  drawing them in order puts children on top.

## App

### UI (`ui/app.slint`)

The whole interface is one Slint file. Rust feeds it plain structs
(`TreeRow`, `MapRect`, `FileRow`, `TypeRow`, `LocationRow`) and receives
callbacks.

- **Wide layout** (≥ 760 px): folder tree on the left; treemap, largest files
  and file types in tabs on the right.
- **Compact layout** (phones, narrow windows): four tabs, larger touch rows, no
  percent column.
- `compact` is set from a `changed width` handler rather than bound to
  `width`. A binding would make the window's size depend on itself through the
  layout.
- The treemap is a list of absolutely positioned rectangles computed in Rust.
  Its size is reported back through `map-resized` whenever the pane changes.

### Controller (`controller.rs`)

`Controller` holds the state: the current tree, size mode, selection, the
focused folder (what the treemap and lists show) and any background job. It is
shared as `Rc<RefCell<Controller>>`. UI callbacks borrow it through
`Controller::run`; if it's already borrowed (a callback fired while Rust was
updating the UI), the job is retried on the next event loop turn instead of
panicking.

Long-running work (scanning, deleting) runs on a plain thread and writes its
result into an `Arc<Mutex<Option<T>>>`. A 100 ms Slint timer polls those
results and the scan progress, and stops itself when nothing is pending. That
keeps all UI updates on the UI thread with no `Send` requirements on UI state.

Interaction rules:

- Selecting a folder (in the tree, treemap or lists) makes it the focus: the
  treemap zooms into it and the lists cover it.
- Selecting a file keeps the current focus if the file is inside it.
- Selecting from the treemap or lists expands the tree down to the item and
  scrolls to it.

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
skipped.

`AppReport::breakdown` divides the data partition (`statvfs`) the way Android
Settings does:

- Apps: code plus data. Preinstalled apps that were never updated count only
  their data, because their code lives on the system partition.
- Your files: the shared storage figure above.
- System & other: what's left of the used space.
- Free.
- Separately, the advertised capacity minus the partition size is reported as
  Android's own share outside user storage.

`home.rs` turns this into the home screen's rows: drives, the breakdown bar,
then every app largest first. Tapping an app opens
`Settings.ACTION_APPLICATION_DETAILS_SETTINGS` for it. No app can clear
another app's cache on modern Android, so the user does that there.

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
- **Signing**: `apksigner` adds v2/v3 signatures with the keystore from
  `CARGO_APK_RELEASE_KEYSTORE`, or a throwaway key if none is set.

`scripts/android/check-apk.sh` checks the result and fails the build if any of
these doesn't hold:

- both ABIs are present;
- minSdk is at least 26 and targetSdk at least 35;
- the launcher activity is exported and the APK isn't debuggable;
- there is a v2/v3 signature;
- every LOAD segment of every `.so` is aligned to 16 KB, and the zip passes
  `zipalign -c -P 16`.

## Testing

- `cargo test --workspace` runs the core tests (sizes, contiguity, hard links,
  symlinks, permissions, cancellation, deletion, squarify geometry) and the
  app's helper tests.
- CI runs them on Windows, macOS and Linux, and builds and checks the Android
  APK with `scripts/android/build-apk.sh`.

## Possible next steps

- **Windows MFT reader**: reading the NTFS Master File Table directly is how
  WizTree scans a whole drive in seconds. It needs admin rights, so it would be
  an optional fast path next to the current scanner.
- **Android app sizes** via `StorageStatsManager`. It needs the usage-access
  permission and fills the "apps" gap that the filesystem can't show.
- **Scan history and compare** ("what grew since last week").
- **Duplicate finder** (group by size, then hash).
- **Export** to CSV or JSON.
- **macOS `.app` bundle and Windows installer** in the release workflow.

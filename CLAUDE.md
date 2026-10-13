# Working on FreeSize

FreeSize (DiskTree until 0.8.6) is a disk space analyzer (Rust + Slint) for Windows, macOS, Linux
and Android, with a self-hosted Hub for all devices and an Unraid plugin.
Read [README.md](README.md) for what it does, [ARCHITECTURE.md](ARCHITECTURE.md)
for how, and [docs/ROADMAP.md](docs/ROADMAP.md) for what's done and next.

## Rules from the owner (always)

- **Never delete anything that would break a device or server, and nothing
  is ever deleted automatically.** Scans and rules only suggest. The person
  can always see the exact paths, and open them to look at the files, before
  anything is deleted. Prefer the quarantine (moved aside, put back later)
  over deleting.
- **Destructive actions are approved on the device itself**, for just that
  request or for N minutes; the lengths on offer are set per device group in
  the Hub's settings.
- **A device's approver can't be changed from the Hub.** Only on the device:
  `freesize approver set` on the command line, the app, or the Unraid
  plugin's settings page.
- **The Hub signs people in with OpenID Connect through Authentik.**
- **Don't suggest growing the build VM's disk.** When space runs low, clean
  build outputs instead (other targets under `target/`, old `dist/` folders).

## How we work

- One branch, `master`: commit and push there. The owner wants no other
  branches (2026-10-13). No pull requests unless asked.
- Before pushing: `cargo fmt --all`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`. For changes to shared code,
  also clippy for Windows (`--target x86_64-pc-windows-gnu`) and macOS
  (`--target aarch64-apple-darwin --exclude freesize-hub`). For Android
  changes, `scripts/android/build-apk.sh dist/test.apk` builds and checks
  the APK.
- Keep README.md (the user page), docs/DEVELOPING.md (the developer page),
  ARCHITECTURE.md and docs/ROADMAP.md in step with changes.
- The repository is public, history included: never commit keys, tokens,
  passwords, or the owner's addresses, hostnames or other personal details.
- Each release bumps `version` in the root `Cargo.toml`; Android won't
  install a lower version over a higher one.
- `cargo run -p freesize-hub --example demo` starts a Hub with made-up
  devices on http://127.0.0.1:9090 for trying dashboard changes.

## This machine

This Ubuntu VM is the build machine, with the repository in
`~/TreeSizeCloneAndroid` (its name on GitHub until 2026-10-12; it's
McGeaverBeaver/FreeSize now, and public). `scripts/release.sh` runs the tests, builds
everything (APK, Linux, Windows, the Hub, the Unraid plugin) and publishes
the GitHub release; `--build-only` just builds into `dist/`, `--draft`
publishes a draft. A release needs about 15 GB of free disk. The Android
signing key and its password are in `~/.android/`: never print, copy, move
or commit them. The release needs `musl-tools` (for the Unraid plugin) since
0.8.0, and since 0.8.2 Docker with buildx (bobby in the `docker` group;
`sg docker -c '...'` works before logging in again) and
`gcc-aarch64-linux-gnu` for the Hub's Docker image, which it pushes to
ghcr.io with the GitHub CLI's token, tagged `<version>` and `latest` on
every release, drafts too: the owner publishes drafts on GitHub's website.
`--publish` makes a draft public (or finds it public already) and points
`latest` at it again. Docker's storage is on the small root disk
(the big disk is a 9p share it can't use), so images must not be built by
compiling inside Docker.

## Where things stand (0.16.1)

- 0.8.0: the Unraid plugin (phase 4). 0.8.1: the Android "App caches" card
  is honest: Android's own cache cleaner only empties the cache folders in
  shared storage, and the card now says how much that is, offers the
  cleaner only when it's worth it, and otherwise points to Biggest caches.
- 0.8.2: each Android app group (Biggest caches, Not opened in 3 months, …)
  opens as its own page with a back arrow; Back returns to the same spot on
  the home screen. Before, a tap changed a list further down the screen and
  seemed to do nothing. Rows also show when they're pressed.
- 0.8.0 and 0.8.1 were never published; 0.8.2, which contains them, is
  public. (It was deleted by mistake once and put back with the same files
  and regenerated notes.)
- 0.8.3: the same Android app as 0.8.2; the first release with the Hub's
  Docker image and ARM (aarch64) downloads of the Hub and command line.
  The Unraid page fits the screen, groups Docker's findings, and says it's
  still looking (which disk, of how many) until the first look is done.
  Moving between disks: a folder picker, several source disks, and a
  queue in the daemon so moves run one at a time.
- 0.8.4 and 0.8.5: see docs/ROADMAP.md. 0.8.6: DiskTree became FreeSize;
  everything from before carries over (data folders, `DISKTREE_…`
  settings, the old Authentik groups, `disktree://` links; on Unraid the
  old plugin's last update installs `freesize.plg`). The quarantine folder
  and the protocol's wire names keep the old name. 0.8.7: the Hub serves
  HTTPS with its own certificate authority by default, plain http on the
  same port for the health check and for older devices, which are told to
  move in a signed answer; `FREESIZE_TLS=off` for a reverse proxy. 0.8.8:
  scans name the folders they couldn't read (Clean up tab, command line,
  Hub) and offer the fuller scan: Windows relaunches as administrator
  through UAC, macOS and Linux scan as root in the background
  (`freesize-gui --export`, through an administrator prompt or `pkexec`)
  and open the result read-only; the dashboard flags incomplete scans.
  0.9: GPL-3.0-or-later, SECURITY.md, TESTING.md, issue templates, CI;
  signed release sums (`freesize release key|sign|verify`, the public half
  in `release/public-key.txt`, empty until the owner makes the key); the
  update check and updater (opt-in in the app's Updates section, or the
  Hub checks with `FREESIZE_UPDATE_CHECK=on` and tells older devices);
  the quarantine in the desktop and Android apps; Hub limits on pairing
  and sign-in, `FREESIZE_RETENTION_DAYS`, the audit head in the log and in
  alerts, "older version" on device cards. 0.9.1: the Community
  Applications files in `unraid/ca/` (plus categories and the license in
  the Hub's template) and the README split into the user page and
  `docs/DEVELOPING.md`. 0.10: the junk cleaner (`freesize_core::junk`,
  rules per platform with fixture tests; the Clean junk screen on
  desktops, `freesize junk`, the Hub's Junk tab, alert and request, the
  phone's new cards, the Unraid trash folders). 0.11: the plugin's
  Nextcloud report (`crates/unraid/src/nextcloud.rs`), the mover's sparse
  copies, extended attributes, partial cleanup, empty-after tick and
  per-file progress, the Empty a disk and Gather a share presets, scans
  indexed per disk so trees are dropped (`scans.rs`), peak memory in the
  log, and btrfs, APFS and purgeable space in the hidden space notes.
  0.12: copies, read only: devices report their Nextcloud sync folders
  (`freesize_agent::nextcloud`, `freesize nextcloud`), the Hub connects a
  Nextcloud account through Login Flow v2 and lists it over WebDAV
  (`crates/hub/src/nextcloud.rs`), and the Copies page joins them
  (`copies.rs`): kept on N devices per folder and big file, and the "only
  one copy" report. The Hub reads only. 0.13: copies, actions: the
  Suggestions tab and Free/Keep here buttons (online-only through the pin
  attributes on Windows, camera files to the phone's quarantine, moving a
  folder into the sync folder with a checked copy, external storage on
  Unraid through `occ` in the container; MKCOL and the free quota on the
  Hub). 0.14: the phone in the background: the Java `HubJob` (a
  `JobService` every 15 minutes, `crates/app/android/java`) calls
  `background.rs`, which sends kept and fresh reports and turns requests
  into notifications; `build-apk.sh` compiles the Java and repacks
  cargo-apk's APK with the hand-written manifest. Shizuku is left over.
  0.15: leftovers of removed programs on desktops
  (`freesize_core::leftovers`): a check-first junk rule from the
  installed-programs list (registry, Spotlight, `.desktop` files and
  package databases) and a record of which folder belonged to which
  program (`programs.json`); id-named folders (Flatpak, snap, bundle ids)
  without the record. 0.15.1: a QR code scanned while the Android app
  was open only brought it to the front; the link filter is now on the
  Java `PairLink` activity, which hands the link over through a file;
  and "check first" junk rules can be ticked on desktops (they couldn't).
  0.16: the Clean up tab redesigned for everyday use: cards opening to
  pages with tick boxes, "select extra copies" (by file type too), similar
  photos as tiles with "all but the largest", ticking fills the basket
  (called the selection), and the Hub's Junk tab ticks several rules.
  0.16.1: nothing could be deleted on a phone: the quarantine went at the
  top of shared storage's filesystem (`/storage/emulated`, not writable)
  instead of `/storage/emulated/0`.
- Not yet done:
  1. 0.8.3 is public (2026-10-12), with the Hub's image public on ghcr.io
     and linked to the repository. Claude Code runs as Remote Control in
     a tmux session; it only reaches Docker if that tmux was started from
     a login made after bobby joined the `docker` group.
  2. The owner installed the plugin (0.8.2) on their server on
     2026-10-12: the tabs and Docker's findings showed. Still to check
     there: `/var/log/disktree.log`, `disktree unraid status`, and 0.8.3's
     page. Unraid downloads the plugin from the GitHub release, which
     works since the repository went public. Unraid's own CSS is in
     github.com/unraid/webgui (`emhttp/plugins/dynamix/styles`); render
     the page against it before changing its layout.
  3. The roadmap's Next releases are all done as of 0.14, and 0.15 took
     the leftovers item from "Left over"; what's left there is Shizuku
     mode, the Hub on Windows and the parked items (docs/ROADMAP.md).
     0.15 leaves the owner the leftovers check in TESTING.md on a
     Windows machine and a Linux box. 0.14 leaves the owner the day-long
     background check in TESTING.md, and the APK has to be built on the
     VM: this container type-checks the Android code but can't build the
     APK. 0.13 leaves the owner the Suggestions checks in
     TESTING.md (the 10 GB folder online-only and back, the camera files,
     a move across drives, external storage). 0.12 leaves the owner the
     Copies checks in TESTING.md (20 files on three devices, a file kept
     on one device only). 0.11 leaves the owner
     the Nextcloud, sparse vdisk and peak memory checks in TESTING.md on
     the real server. 0.10 leaves the owner the junk checks in TESTING.md on a
     Windows machine, a Linux box and a phone. 0.9.1 leaves the owner the CA submission
     (Squid's form, 2FA on GitHub, a forum support thread if asked) and a
     check on a clean Unraid 7.3. 0.9 leaves the owner three things: `freesize release key` on the build
     machine and commit `release/public-key.txt`; register the package
     and the Android key with Google; try the updater and the phone's
     quarantine on real devices (TESTING.md).
  4. 0.8.6 to 0.14.0 haven't been tried on real devices yet: the Unraid
     update through `disktree.plg`, the phone pairing again under the new
     package id, HTTPS pairing from the QR code, a 0.8.6 device moving
     over to https, and the administrator and root scans (UAC on Windows,
     the password prompts on macOS and Linux), the in-app update and the
     phone's quarantine.

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

- One branch, `santaslittlehelper/nifty-darwin-a8pxxr`: commit and push
  there. No pull requests unless asked.
- Before pushing: `cargo fmt --all`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`. For changes to shared code,
  also clippy for Windows (`--target x86_64-pc-windows-gnu`) and macOS
  (`--target aarch64-apple-darwin --exclude freesize-hub`). For Android
  changes, `scripts/android/build-apk.sh dist/test.apk` builds and checks
  the APK.
- Keep README.md, ARCHITECTURE.md and docs/ROADMAP.md in step with changes.
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

## Where things stand (0.8.8)

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
  3. Next on the roadmap (docs/ROADMAP.md, Next releases): 0.9 trust and
     updates, 0.9.1 Community Applications, 0.10 the junk cleaner, 0.11
     where the space goes, 0.12 and 0.13 Nextcloud, 0.14 the phone in the
     background.
  4. 0.8.6 to 0.8.8 haven't been tried on real devices yet: the Unraid
     update through `disktree.plg`, the phone pairing again under the new
     package id, HTTPS pairing from the QR code, a 0.8.6 device moving
     over to https, and the administrator and root scans (UAC on Windows,
     the password prompts on macOS and Linux).
  5. No license chosen yet.

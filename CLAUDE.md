# Working on DiskTree

DiskTree is a disk space analyzer (Rust + Slint) for Windows, macOS, Linux
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
  `disktree approver set` on the command line, the app, or the Unraid
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
  (`--target aarch64-apple-darwin --exclude disktree-hub`). For Android
  changes, `scripts/android/build-apk.sh dist/test.apk` builds and checks
  the APK.
- Keep README.md, ARCHITECTURE.md and docs/ROADMAP.md in step with changes.
- The repository is public, history included: never commit keys, tokens,
  passwords, or the owner's addresses, hostnames or other personal details.
- Each release bumps `version` in the root `Cargo.toml`; Android won't
  install a lower version over a higher one.
- `cargo run -p disktree-hub --example demo` starts a Hub with made-up
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
ghcr.io with the GitHub CLI's token. `--publish` makes a draft public and
moves the image's `latest` tag. Docker's storage is on the small root disk
(the big disk is a 9p share it can't use), so images must not be built by
compiling inside Docker.

## Where things stand (0.8.3)

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
- 0.8.3: the same app as 0.8.2; the first release with the Hub's Docker
  image and ARM (aarch64) downloads of the Hub and command line.
- Not yet done:
  1. Build 0.8.3 with `scripts/release.sh --draft`, have the owner make the
     ghcr.io package public once (Unraid pulls without logging in), then
     `scripts/release.sh --publish`. Claude Code runs as Remote Control in
     a tmux session; it only reaches Docker if that tmux was started from a
     login made after bobby joined the `docker` group.
  2. The Unraid plugin has only been tested against a made-up server
     folder. On the first real install, check that Tools → Disk Space →
     DiskTree appears, `/var/log/disktree.log`, and
     `disktree unraid status`. Unraid downloads the plugin from the
     GitHub release, which works since the repository went public.
  3. Next on the roadmap: phase 5 (Nextcloud and "do I have a copy?"),
     then phase 6 (a CCleaner-style junk cleaner on every device, every
     item with a blurb on what cleaning it means), plus the held-back items
     listed under Done in docs/ROADMAP.md.
  4. No license chosen yet.

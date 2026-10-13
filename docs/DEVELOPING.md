# Developing FreeSize

How to build FreeSize, try the Hub, and make a release. What it does is in
the [README](../README.md); how the pieces work in
[ARCHITECTURE.md](../ARCHITECTURE.md); what's done and next in
[ROADMAP.md](ROADMAP.md); what to try by hand before a release in
[TESTING.md](../TESTING.md); what leaves a device and how to report a
problem in [SECURITY.md](../SECURITY.md). Bug reports and ideas go in
GitHub issues, which have templates for both. The license is
GPL-3.0-or-later.

## Building

Everything builds on your own machine. You need a recent stable Rust (1.92+)
from [rustup.rs](https://rustup.rs).

- **Windows**: run `rustup-init.exe` and let it install the Visual Studio C++
  build tools when it asks.
- **Linux** also needs a C compiler and the fontconfig headers:

  ```sh
  sudo apt install build-essential pkg-config libfontconfig1-dev   # Debian/Ubuntu
  sudo dnf install gcc pkgconf fontconfig-devel                    # Fedora
  ```

Get the code with `git clone https://github.com/McGeaverBeaver/FreeSize`
and run the commands below inside that folder. `git pull` fetches updates.

### Desktop app

```sh
cargo run --release -p freesize-app            # opens the drive picker
cargo run --release -p freesize-app -- ~/Videos   # scans a folder right away
```

The binary is `target/release/freesize-gui` (`freesize-gui.exe` on Windows).
It's a single file you can copy anywhere. On Windows, release builds don't open
a console window. `cargo build --release -p freesize-app -p freesize-cli`
builds the app and the command line without starting anything.

### Command line

`cargo build --release -p freesize-cli` builds `target/release/freesize`;
its use is in the [README](../README.md#command-line).

## Trying the Hub without devices

```sh
cargo run -p freesize-hub --example demo
```

starts a Hub on http://127.0.0.1:9090 with a stand-in sign-in (it signs you
straight in as an admin) and a few made-up devices, scans, requests and
alerts.

## Releases

Releases are built on your own Linux machine (an Ubuntu VM works well) and
published as GitHub releases, with the APK signed by your own key so every
release installs over the previous one.

### Setting up a build machine (once)

On a fresh Ubuntu Server (24.04 or newer):

```sh
sudo apt update && sudo apt upgrade -y
sudo apt install -y build-essential pkg-config git curl unzip zip gh mingw-w64 \
    musl-tools openjdk-21-jdk-headless libfontconfig1-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
git clone https://github.com/McGeaverBeaver/FreeSize.git
cd FreeSize
scripts/android/setup-linux.sh   # Android SDK, NDK, cargo-apk
gh auth login                    # GitHub.com, HTTPS, log in with a web browser
```

`gh auth login` shows a code and a link; open the link on any computer, enter
the code, and the machine can publish releases to your repositories.

### Disk space

Give the build machine at least 40 GB of disk and 8 GB of RAM. A release needs
about 15 GB for build files: Rust builds for Linux, Windows and two Android
targets, plus the Android SDK and NDK. When the disk fills up during a build,
the linker fails with "Bus error" or "No space left on device".

`df -h ~` shows how much is free. To make room:

- **The virtual disk is too small** (the filesystem is a partition such as
  `/dev/vda2` or `/dev/sda2` and fills the whole disk): enlarge the disk in
  your VM software, then grow the partition and filesystem into the new
  space. `lsblk` shows the names; the system partition must be the last one
  on the disk.

  ```sh
  sudo growpart /dev/vda 2      # disk, then partition number
  sudo resize2fs /dev/vda2      # ext4; for XFS: sudo xfs_growfs /
  ```

- **Ubuntu Server left part of the disk unused** (the filesystem is
  `/dev/mapper/ubuntu--vg-ubuntu--lv`, which the installer sizes to part of
  the disk by default):

  ```sh
  sudo lvextend -r -l +100%FREE /dev/ubuntu-vg/ubuntu-lv
  ```

`cargo clean` deletes all build files when space runs low; the next build
then takes longer.

**Building on a shared folder.** A VM can keep the project on a folder its
host shares over 9p, such as an Unraid share with mount tag `ssdbobbybuilds`.
Mount it with `cache=mmap`: the linker needs memory-mapped files, which 9p
doesn't fully support without caching, while `cache=loose` (which also
caches writes) returned corrupted file contents in a real build.

```sh
sudo mkdir -p /mnt/ssdbuilds
echo 'ssdbobbybuilds /mnt/ssdbuilds 9p trans=virtio,version=9p2000.L,msize=262144,cache=mmap,nofail 0 0' | sudo tee -a /etc/fstab
sudo systemctl daemon-reload && sudo mount /mnt/ssdbuilds
git clone https://github.com/McGeaverBeaver/FreeSize.git /mnt/ssdbuilds/FreeSize
ln -s /mnt/ssdbuilds/FreeSize ~/FreeSize
```

To check that every file arrived intact, make git re-read them all; this
should print nothing:

```sh
cd ~/FreeSize
find . -path ./.git -prune -o -type f -exec touch {} +
git status --short
```

Rust, the Android SDK and the signing key stay on the VM's own disk.

### Publishing a release

1. Bump `version` in the root `Cargo.toml` (for example to `0.2.1`) and push.
   The Android version code is derived from it, and Android won't install a
   lower version over a higher one.
2. On the build machine:

   ```sh
   cd ~/FreeSize
   git pull
   scripts/release.sh
   ```

`scripts/release.sh` runs the tests, builds `freesize-android.apk`,
`freesize-linux-x86_64.tar.gz`, `freesize-hub-linux-x86_64.tar.gz`,
`freesize-windows-x86_64.zip` (the Windows build is cross-compiled with
MinGW) and the Unraid plugin, `freesize.plg` with its package
`freesize-<version>-x86_64-1.txz` (a static musl build, so it runs on every
Unraid version), writes `SHA256SUMS.txt`, and creates the GitHub release `v<version>`
for the checked-out commit with those files and notes listing the changes.
With `gcc-aarch64-linux-gnu` installed it adds the Hub and command line for
ARM boards (`*-linux-aarch64.tar.gz`).

It also publishes the Hub's Docker image, as a GitHub Actions workflow
would but without using Actions minutes: `ghcr.io/mcgeaverbeaver/freesize-hub`,
a package of the repository, for amd64 and arm64. The image takes the Hub
the release has just built, so Docker compiles nothing and needs only a few
hundred MB. It logs in to GitHub's registry with the GitHub CLI's token
(which needs the `write:packages` scope), in a throwaway Docker config so
the token isn't left on disk. Once, on the build machine:

```sh
sudo apt install docker.io docker-buildx gcc-aarch64-linux-gnu
sudo usermod -aG docker $USER   # then log in again
```

and the first time the image is pushed, make the package public on GitHub
(Packages → freesize-hub → Package settings → Change visibility) so Unraid
can pull it without logging in; the script reminds you while it's private.
`FREESIZE_IMAGE` names another image, and `--no-image` leaves it out.

The script refuses to publish uncommitted or unpushed code, or a version
that's already released. The image is tagged `<version>` and `latest` (the
one Unraid pulls) on every release, drafts included, so publishing a draft
on GitHub's website needs nothing more. `--draft` publishes a draft to
check first; `scripts/release.sh --publish` makes it public and points
`latest` at it again, also when it was made public on the website. `--build-only` just builds into `dist/` (and builds the image
without pushing it). macOS builds need a Mac, so releases have none.

The GitHub Actions CI workflow (`.github/workflows/ci.yml`) no longer runs on
its own. It runs formatting, clippy and tests on Windows, macOS and Linux,
builds the APK and the Unraid plugin, when started by hand from the Actions
tab. The same checks locally:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### Release key

Releases are signed, so the apps can tell a release that came from the
build machine from one that didn't. Once, on the build machine:

```sh
freesize release key          # makes ~/.freesize/release-key, prints the public half
```

Paste the printed public half into `release/public-key.txt` and commit it;
from then on `release.sh` signs every release's `SHA256SUMS.txt`
(`SHA256SUMS.txt.sig` beside it) with `freesize release sign` and checks
it with `freesize release verify`. Without the key file, the release is
built unsigned and the script says so: it still works, but apps won't
install it themselves. `freesize release verify SHA256SUMS.txt` checks a
download by hand. The private key never leaves the build machine (mode
0600, outside the repository); if it's lost or leaks, make a new one and
commit the new public half, and the next release is installed by hand once.
See [release/README.md](release/README.md).

### Android signing key

Android only installs an update if it's signed with the same key as the
installed app. On the first build, `build-apk.sh` creates
`~/.android/disktree-release.keystore` and a random password in
`~/.android/disktree-release.keystore.password`, and signs every later build
and release with them.

**Back up both files** (a password manager, a USB stick). If you lose them,
nobody can publish an update that installs over the app: everyone would have
to uninstall it and set up its permissions again. To build on another
machine, copy both files into its `~/.android/`. From Windows:

```powershell
scp you@build-machine:~/.android/disktree-release.keystore* .
```

To sign with a different key, set `CARGO_APK_RELEASE_KEYSTORE` and
`CARGO_APK_RELEASE_KEYSTORE_PASSWORD`.

## Project layout

```
crates/core   scanner, tree model, stats, treemap and bubble layout,
              snapshots, scan files (no UI)
crates/cli    `freesize` command-line tool and Hub agent
crates/app    Slint app; ui/app.slint is the whole UI, src/controller.rs the logic
crates/proto  what devices and the Hub say to each other: keys, signatures,
              pairing links, reports, commands
crates/agent  the device side of the Hub, shared by the app and the agent
crates/hub    FreeSize Hub: web server, dashboard (web/), SQLite, alerts
crates/unraid Unraid: the array, shares, Docker, VMs, what to protect,
              disk activity, permissions, moves between disks
unraid/       the Hub's Unraid template, ca/ (the Community Applications
              profile and the plugin's entry), and plugin/: the .plg and
              the package (page, start script)
packaging/    the Hub's Docker entrypoint, systemd units
```

`docs/agents/` tells coding agents using Matt Pocock's engineering skills
where issues live (GitHub Issues), which triage labels to use, and where the
domain docs go (`CONTEXT.md` and `docs/adr/`, written as terms and decisions
get settled).

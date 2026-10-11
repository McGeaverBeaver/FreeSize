# DiskTree

A fast disk space analyzer in the spirit of TreeSize, written in Rust with a
[Slint](https://slint.dev) UI. One codebase runs on **Windows, macOS, Linux and Android**.

![DiskTree on the desktop](docs/screenshots/desktop.png)

| Phone layout: folders | Phone layout: treemap |
|---|---|
| ![](docs/screenshots/phone-folders.png) | ![](docs/screenshots/phone-map.png) |

> "DiskTree" is a working name. TreeSize is a trademark of JAM Software, so
> don't ship under that name.

## Features

- **Parallel scanner**: about 190k files in 0.2 s on a warm cache. Scanning a
  whole drive is limited by the disk, not the CPU.
- **Folder tree** sorted by size, with percentage bars. Folders expand in place,
  and very large folders fold their smallest items into one row.
- **Treemap** of the selected folder, three levels deep. Click a block to zoom
  in, and use **Up** to zoom out.
- **Largest files** and **file types** (size by extension) for the selected folder.
- **Size on disk vs. file size**, switchable. Compressed, sparse and cloud-only
  files differ a lot between the two.
- **Delete**: moves to the Recycle Bin or Trash on desktop, and permanently
  deletes (after confirming) on Android. Totals update without rescanning.
- **Show in folder** opens Explorer, Finder or the Linux file manager.
- Flags unreadable folders, other filesystems, cloud-only files (OneDrive,
  iCloud) and hard links. Hard links are counted once.
- **Phone layout** with tabs below 760 px width, so it also works on small
  desktop windows.
- **Apps & system breakdown (Android)**: how the whole phone's storage
  divides up between apps, your files, the system and free space. Every app is
  listed with its code, data and cache sizes.
- **App cleanup (Android)**: apps you haven't opened in 3 months or never
  opened, apps no store can give back, and a history of removed apps. Tap an
  app to see when you last used it and where it came from, then uninstall,
  archive, save its APK or clear its cache. See
  [Cleaning up apps](#cleaning-up-apps).
- **Command-line tool** (`disktree`) sharing the same core.

## Platform notes

| Platform | What gets scanned | Notes |
|---|---|---|
| Windows | Any drive or folder | Detects OneDrive placeholders; asks Windows for the real size of compressed and sparse files. |
| macOS | Everything, once Full Disk Access is granted | Grant it in System Settings → Privacy & Security for the full picture. Detects iCloud "dataless" files. |
| Linux | Anything you can read | Stays on one filesystem by default (skips `/proc`, other mounts) and counts hard links once. |
| Android 8.0+ | All shared storage and SD cards, plus sizes for every app and the system | Files need **All files access** (Android 11+). Apps' private data can't be opened as files on Android, so it's measured through Android's storage statistics instead, which need **Usage access**. |
| iOS | — | Not planned: the iOS sandbox doesn't allow scanning the device. |

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

Get the code with `git clone https://github.com/McGeaverBeaver/TreeSizeCloneAndroid`
and run the commands below inside that folder. `git pull` fetches updates.

### Desktop app

```sh
cargo run --release -p disktree-app            # opens the drive picker
cargo run --release -p disktree-app -- ~/Videos   # scans a folder right away
```

The binary is `target/release/disktree-gui` (`disktree-gui.exe` on Windows).
It's a single file you can copy anywhere. On Windows, release builds don't open
a console window. `cargo build --release -p disktree-app -p disktree-cli`
builds the app and the command line without starting anything.

### Command line

```sh
cargo run --release -p disktree-cli -- /usr --depth 2 --top 10
```

```
Scanned /usr in 0.64s: 83,965 files, 8,842 folders
Total: 4.31 GB (4.53 GB size on disk)

  4.53 GB  100.0%  ████████████████████  /usr
  2.17 GB   47.9%  █████████▋            ├── lib/
  1.12 GB   24.6%  ████▉                 ├── local/
   624 MB   13.5%  ██▊                   ├── bin/
```

Run `disktree --help` for all options (`--apparent` for file sizes,
`--cross-filesystems`, `--min-percent`, …).

### Android

The APK builds on Linux, or on Windows inside WSL: run `wsl --install` once in
an administrator PowerShell, restart, and use the Ubuntu terminal it adds. In
WSL, keep the repository in your Linux home folder rather than under `/mnt/c`,
which is much slower.

One-time setup (Debian/Ubuntu and WSL; other distros need the same packages):

```sh
sudo apt install curl unzip openjdk-17-jdk-headless build-essential   # or openjdk-21-…
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # if you don't have Rust
git clone https://github.com/McGeaverBeaver/TreeSizeCloneAndroid
cd TreeSizeCloneAndroid
scripts/android/setup-linux.sh
```

`setup-linux.sh` downloads the Android command-line tools, SDK platform 36,
build tools, NDK and adb into `~/Android/Sdk` (a few GB), then adds the Rust
Android targets and `cargo-apk`. It doesn't need root. To use an existing SDK,
for example Android Studio's, set `ANDROID_HOME` to it first.

Build:

```sh
git pull
scripts/android/build-apk.sh      # -> target/release/apk/DiskTree.apk
```

The first build takes several minutes; later ones only rebuild what changed.
It also creates the signing key that every later build reuses (see
[Android signing key](#android-signing-key)), so new builds install over the
old one and keep DiskTree's permissions. Releases up to 0.1.3 were signed
with a different key, so uninstall such a copy once first.

To get the APK onto the phone, either:

- copy it over (USB cable, Google Drive, …) and open it on the phone. From
  WSL, `cp target/release/apk/DiskTree.apk /mnt/c/Users/<you>/Downloads/` puts
  it in your Windows Downloads folder.
- or install it over Wi-Fi with adb. Turn on Developer options (Settings →
  About phone → tap *Build number* seven times), then Settings → System →
  Developer options → *Wireless debugging* → *Pair device with pairing code*:

  ```sh
  adb=~/Android/Sdk/platform-tools/adb
  $adb pair 192.168.1.50:37123      # address and code from the pairing dialog
  $adb connect 192.168.1.50:41234   # address on the Wireless debugging screen
  $adb install -r target/release/apk/DiskTree.apk
  ```

`build-apk.sh` produces one APK for every device: arm64 phones plus x86_64
emulators and Chromebooks. It links the native code for 16 KB memory pages and
runs `scripts/android/check-apk.sh`, which fails the build if the APK would be
rejected by a phone or by Google Play. To see the app's log, run
`adb logcat -s DiskTree`.

Publishing to Google Play requires declaring why the app needs All files access
and why it lists all installed apps (`QUERY_ALL_PACKAGES`). Disk analyzers and
file managers are an accepted use, but it goes through review. Sideloading and
F-Droid don't have that step.

## Installing on an Android phone

1. Download **`disktree-android.apk`** from the latest release. It's one file
   for all phones.
2. Open it. If Android asks, allow your browser or Files app to install unknown
   apps.
3. Google Play Protect may warn that the app comes from an unknown developer.
   Tap *More details → Install anyway*.
4. Start DiskTree, tap **Grant access**, turn on *Allow access to manage all
   files*, and go back. Then pick *Internal storage* or an SD card.
5. To see what apps and the system use, tap **Allow usage access** under
   *Apps & system*, turn DiskTree on in the list, and go back.
6. If the switch is greyed out or Android says *App was denied access* /
   *Controlled by Restricted Setting*, Android is blocking it because the app
   was installed from a downloaded file. Tap **Open DiskTree's app info** in
   DiskTree (or go to Settings → Apps → DiskTree), tap **⋮** in the top
   corner, choose **Allow restricted settings** and confirm with your PIN or
   fingerprint. Then repeat step 5. You only need to do this once per install.

### Why a file scan finds less than the phone uses

Android only lets an app read shared storage: photos, downloads, music and the
like. Most of a phone's space is somewhere no app can open as files:

- each app's private data (offline music and videos, chats, game data, caches)
  and its folders under `Android/data` and `Android/obb`;
- installed app code;
- system data, other user profiles and temporary files.

On top of that, the Android OS has partitions of its own outside user
storage. That's why a 128 GB phone shows about 109 GB.

The *Apps & system* section covers all of this with the same storage
statistics Android's Settings app uses:

- Apps: each app's code plus data. Preinstalled apps' code sits on the system
  partition, so only their data counts.
- Your files: shared storage that belongs to no app.
- System & other: the rest of the used space.

Tapping an app opens its settings page, where you can clear its cache, clear
its storage or uninstall it.

The Back gesture steps back through the app: it closes a dialog, stops a scan,
zooms the treemap out, then returns to the drive list before leaving the app.

### Cleaning up apps

Under the storage breakdown, the apps are grouped so the space you can win
back is easy to find:

| Group | What's in it |
|---|---|
| All apps | Every app, biggest first. |
| Not opened in 3 months | Apps you haven't had on screen for 3 months. |
| Never opened | Apps with no use recorded since they were installed (Android keeps about 2 years of usage history). |
| Not from an app store | Apps installed from a downloaded file or a computer. No store can give them back, so save the APK first. |
| Removed apps | Apps DiskTree has seen before that are gone, so you can find one again by name. |

Apps that work without being opened are never suggested for removal:
preinstalled apps, apps without an icon, your default home screen, browser,
messaging and phone apps, keyboards, accessibility services, apps that read
notifications (such as watch apps), device admin apps, and archived apps.

Tapping an app shows its size, when it was installed and last opened, its time
on screen over the last 30 days, where it came from, and tips for a few
well-known apps (for example that clearing Signal's storage deletes your
messages). Its buttons:

- **Uninstall**: Android asks you to confirm.
- **Archive** (Android 15+, Play Store apps): opens the app's info page, where
  *Archive* removes the app but keeps its data and login. Tapping its icon
  later reinstalls it from the Play Store.
- **Save APK to Downloads** (apps not from a store): copies the app's install
  file to `Download/DiskTree`, so you can install it again later. The app's
  data isn't included.
- **Open in Play Store** (or the store it came from).
- **App info**: Android's page for clearing the app's cache or data.

**Clear every app's cache** (Android 11+) opens Android's own dialog that
clears all apps' cache at once. It needs All files access. Apps rebuild some of
their cache as they run.

After you come back from uninstalling an app or from its info page, DiskTree
measures the apps again.

The app history is a small file, `Documents/DiskTree/app-history.json`, with a
second copy in DiskTree's private storage. The first survives uninstalling
DiskTree. Other apps with All files access can read it, so delete it if you
don't want the list of your apps there. DiskTree only notices an app was
removed the next time it measures the apps, so the removal date can be later
than the day you removed it.

| Problem | Cause | Fix |
|---|---|---|
| Uninstall does nothing | The APK was built without the `REQUEST_DELETE_PACKAGES` permission (versions before 0.2.0). | Install 0.2.0 or later. |
| *Apps & system* stays at "Allow usage access" | DiskTree isn't switched on under Settings → Apps → Special app access → Usage access. | Turn it on there; the list appears within a second of returning. |
| Usage access switch is greyed out, says "Controlled by Restricted Setting", or shows "App was denied access" | Android 13+ blocks Usage access for apps installed from a downloaded file (restricted settings). | Settings → Apps → DiskTree → ⋮ → *Allow restricted settings*, then turn Usage access on. If ⋮ isn't there, try to turn the switch on once first; the menu only appears after Android has blocked it. |
| "You can't install the app on your device" or "App not compatible" | The APK has no code for the phone's processor. v0.1.0 shipped separate files, and the `x86_64` one is for emulators. | Use `disktree-android.apk` from v0.1.1 or later. |
| "App not installed as package conflicts with an existing package" | The installed copy was signed with a different key: releases up to 0.1.3 were built on GitHub with a throwaway key. | Uninstall DiskTree once, then install the new APK. Releases from 0.2.0 on share one key and install over each other. |
| Installing apps from outside the Play Store is blocked entirely | Advanced Protection is on (Settings → Security & privacy), or a work profile forbids it. | Install with `adb install`, or turn that setting off. |
| "App blocked" because the developer isn't verified | Google is rolling out developer verification region by region. | `adb install` still works. Registering the developer and package `dev.disktree.app` with Google removes the block for everyone. |
| A folder shows "no access" | `Android/data`, `Android/obb` and other apps' private files are off limits to every app on Android 11+. | Nothing to fix; that's an Android rule. |

## Releases

Releases are built on your own Linux machine (an Ubuntu VM works well) and
published as GitHub releases, with the APK signed by your own key so every
release installs over the previous one.

### Setting up a build machine (once)

On a fresh Ubuntu Server (24.04 or newer):

```sh
sudo apt update && sudo apt upgrade -y
sudo apt install -y build-essential pkg-config git curl unzip zip gh mingw-w64 \
    openjdk-21-jdk-headless libfontconfig1-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
git clone https://github.com/McGeaverBeaver/TreeSizeCloneAndroid.git
cd TreeSizeCloneAndroid
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

### Publishing a release

1. Bump `version` in the root `Cargo.toml` (for example to `0.2.1`) and push.
   The Android version code is derived from it, and Android won't install a
   lower version over a higher one.
2. On the build machine:

   ```sh
   cd ~/TreeSizeCloneAndroid
   git pull
   scripts/release.sh
   ```

`scripts/release.sh` runs the tests, builds `disktree-android.apk`,
`disktree-linux-x86_64.tar.gz` and `disktree-windows-x86_64.zip` (the Windows
build is cross-compiled with MinGW), writes `SHA256SUMS.txt`, and creates the
GitHub release `v<version>` for the checked-out commit with those files and
notes listing the changes. It refuses to publish uncommitted or unpushed code,
or a version that's already released. `--draft` publishes a draft to check
first; `--build-only` just builds into `dist/`. macOS builds need a Mac, so
releases have none.

The GitHub Actions CI workflow (`.github/workflows/ci.yml`) no longer runs on
its own. It runs formatting, clippy and tests on Windows, macOS and Linux when
started by hand from the Actions tab. The same checks locally:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

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
crates/core   scanner, tree model, stats, treemap layout (no UI)
crates/cli    `disktree` command-line tool
crates/app    Slint app; ui/app.slint is the whole UI, src/controller.rs the logic
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for how the pieces work.

## Licensing

No license has been chosen for this project yet. Note that Slint is available
under GPLv3, under a royalty-free license for desktop and mobile apps that
requires attribution (for example a "Made with Slint" badge in the About box or
on the website), or under a paid license. Pick the one that matches how you
want to distribute DiskTree.

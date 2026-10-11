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
- **Command-line tool** (`disktree`) sharing the same core.

## Platform notes

| Platform | What gets scanned | Notes |
|---|---|---|
| Windows | Any drive or folder | Detects OneDrive placeholders; asks Windows for the real size of compressed and sparse files. |
| macOS | Everything, once Full Disk Access is granted | Grant it in System Settings → Privacy & Security for the full picture. Detects iCloud "dataless" files. |
| Linux | Anything you can read | Stays on one filesystem by default (skips `/proc`, other mounts) and counts hard links once. |
| Android 8.0+ | All shared storage and SD cards | Needs **All files access** (Android 11+). `Android/data` and other apps' private data stay hidden; that's an Android rule. |
| iOS | — | Not planned: the iOS sandbox doesn't allow scanning the device. |

## Building

You need a recent stable Rust (1.92+).

**Linux** also needs the fontconfig headers:

```sh
sudo apt install libfontconfig1-dev   # Debian/Ubuntu
sudo dnf install fontconfig-devel     # Fedora
```

### Desktop app

```sh
cargo run --release -p disktree-app            # opens the drive picker
cargo run --release -p disktree-app -- ~/Videos   # scans a folder right away
```

The binary is `target/release/disktree-gui`. On Windows release builds it
doesn't open a console window.

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

You need:

- the Android SDK with the `platforms;android-36` package and build tools,
- the NDK,
- JDK 17, and `cargo-apk`.

Android Studio's SDK Manager is the easiest way to get the SDK and NDK.

```sh
export ANDROID_HOME=~/Android/Sdk
export ANDROID_NDK_ROOT=$ANDROID_HOME/ndk/<version>
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-apk

scripts/android/build-apk.sh                       # -> target/release/apk/DiskTree.apk
adb install -r target/release/apk/DiskTree.apk     # phone with USB debugging on
```

`build-apk.sh` produces one APK for every device: arm64 phones plus x86_64
emulators and Chromebooks. It links the native code for 16 KB memory pages and
runs `scripts/android/check-apk.sh`, which fails the build if the APK would be
rejected by a phone or by Google Play. To see the app's log, run
`adb logcat -s DiskTree`.

Publishing to Google Play requires declaring why the app needs All files access.
Disk analyzers and file managers are an accepted use, but it goes through
review. Sideloading and F-Droid don't have that step.

## Installing on an Android phone

1. Download **`disktree-android.apk`** from the latest release. It's one file
   for all phones.
2. Open it. If Android asks, allow your browser or Files app to install unknown
   apps.
3. Google Play Protect may warn that the app comes from an unknown developer.
   Tap *More details → Install anyway*.
4. Start DiskTree, tap **Grant access**, turn on *Allow access to manage all
   files*, and go back. Then pick *Internal storage* or an SD card.

The Back gesture steps back through the app: it closes a dialog, stops a scan,
zooms the treemap out, then returns to the drive list before leaving the app.

| Problem | Cause | Fix |
|---|---|---|
| "You can't install the app on your device" or "App not compatible" | The APK has no code for the phone's processor. v0.1.0 shipped separate files, and the `x86_64` one is for emulators. | Use `disktree-android.apk` from v0.1.1 or later. |
| "App not installed as package conflicts with an existing package" | The installed copy was signed with a different key. | Uninstall DiskTree, then install the new APK. Set up a release key (below) so future updates install over each other. |
| Installing apps from outside the Play Store is blocked entirely | Advanced Protection is on (Settings → Security & privacy), or a work profile forbids it. | Install with `adb install`, or turn that setting off. |
| "App blocked" because the developer isn't verified | Google is rolling out developer verification region by region. | `adb install` still works. Registering the developer and package `dev.disktree.app` with Google removes the block for everyone. |
| A folder shows "no access" | `Android/data`, `Android/obb` and other apps' private files are off limits to every app on Android 11+. | Nothing to fix; that's an Android rule. |

## CI and releases

- **CI** (`.github/workflows/ci.yml`) runs formatting, clippy and tests on
  Windows, macOS and Linux. It also builds and checks the Android APK, which
  you can download from the run's artifacts.
- **Release** (`.github/workflows/release.yml`) runs when you push a tag like
  `v0.1.1`. It builds Windows, macOS and Linux archives plus
  `disktree-android.apk`, and attaches them to a GitHub release.

Before tagging, bump `version` in the root `Cargo.toml`. The Android version
code is derived from it, and Android only installs an update with a higher one.

### Android signing key

Android only installs an update if it's signed with the same key as the
installed app. Without a key, CI signs with a new throwaway key every time, so
each build has to be uninstalled before the next one installs. Create a key
once and store it as two repository secrets
(Settings → Secrets and variables → Actions):

```sh
keytool -genkeypair -keystore disktree-release.keystore -alias release \
    -keyalg RSA -keysize 4096 -validity 10000
base64 -w0 disktree-release.keystore   # value for ANDROID_KEYSTORE_BASE64
```

- `ANDROID_KEYSTORE_BASE64`: the output of the `base64` command.
- `ANDROID_KEYSTORE_PASSWORD`: the password you chose.

Back up the keystore. If you lose it, nobody can update the installed app
without uninstalling it first.

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

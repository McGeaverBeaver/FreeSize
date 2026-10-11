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
  listed with its code, data and cache sizes; tap one to clear its cache or
  data in Android's settings.
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

| Problem | Cause | Fix |
|---|---|---|
| *Apps & system* stays at "Allow usage access" | DiskTree isn't switched on under Settings → Apps → Special app access → Usage access. | Turn it on there; the list appears within a second of returning. |
| Usage access switch is greyed out, says "Controlled by Restricted Setting", or shows "App was denied access" | Android 13+ blocks Usage access for apps installed from a downloaded file (restricted settings). | Settings → Apps → DiskTree → ⋮ → *Allow restricted settings*, then turn Usage access on. If ⋮ isn't there, try to turn the switch on once first; the menu only appears after Android has blocked it. |
| "You can't install the app on your device" or "App not compatible" | The APK has no code for the phone's processor. v0.1.0 shipped separate files, and the `x86_64` one is for emulators. | Use `disktree-android.apk` from v0.1.1 or later. |
| "App not installed as package conflicts with an existing package" | The installed copy was signed with a different key. | Uninstall DiskTree, then install the new APK. Set up a release key (below) so future updates install over each other. |
| Installing apps from outside the Play Store is blocked entirely | Advanced Protection is on (Settings → Security & privacy), or a work profile forbids it. | Install with `adb install`, or turn that setting off. |
| "App blocked" because the developer isn't verified | Google is rolling out developer verification region by region. | `adb install` still works. Registering the developer and package `dev.disktree.app` with Google removes the block for everyone. |
| A folder shows "no access" | `Android/data`, `Android/obb` and other apps' private files are off limits to every app on Android 11+. | Nothing to fix; that's an Android rule. |

## CI and releases

- **CI** (`.github/workflows/ci.yml`) runs formatting, clippy and tests on
  Windows, macOS and Linux. It also builds and checks the Android APK, which
  you can download from the run's artifacts.
- **Release** (`.github/workflows/release.yml`) builds Windows, macOS and
  Linux archives plus `disktree-android.apk`, and attaches them to a GitHub
  release. It runs when a tag starting with `v` or `V` is pushed, which
  includes creating a release with a new tag on GitHub. You can also run it
  without a tag: Actions → Release → Run workflow, pick the branch, and enter
  a tag such as `v0.1.3`. It builds that branch and publishes the release,
  creating the tag.

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

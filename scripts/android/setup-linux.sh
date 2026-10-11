#!/usr/bin/env bash
# One-time setup for building the Android APK on Linux, or on Windows inside
# WSL. Installs into ANDROID_HOME (default ~/Android/Sdk):
#
# - the Android command-line tools, SDK platform 36, build tools, the NDK and
#   platform-tools (adb);
# - the Rust Android targets and cargo-apk.
#
# Needs curl, unzip, a JDK 17+ and Rust (rustup). Nothing needs root. Safe to
# run again; it only adds what's missing.
#
# Usage: scripts/android/setup-linux.sh
set -euo pipefail

ANDROID_HOME="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"
# The versions the APK checks were last run with.
PLATFORM="platforms;android-36"
BUILD_TOOLS="build-tools;37.0.0"
NDK="ndk;30.0.16248370"

missing=()
for tool in curl unzip java keytool cargo rustup; do
    command -v "$tool" >/dev/null || missing+=("$tool")
done
if [ "${#missing[@]}" -gt 0 ]; then
    echo "Missing: ${missing[*]}"
    echo
    echo "Debian/Ubuntu:  sudo apt install curl unzip openjdk-17-jdk-headless   (or openjdk-21-jdk-headless)"
    echo "Fedora:         sudo dnf install curl unzip java-21-openjdk-devel"
    echo "Rust:           curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo "                (then open a new shell)"
    exit 1
fi
java_version="$(java -version 2>&1 | sed -n 's/.*version "\([0-9]*\).*/\1/p' | head -n 1)"
if [ "${java_version:-0}" -lt 17 ]; then
    echo "The Android SDK tools need Java 17 or newer; found ${java_version:-unknown}."
    exit 1
fi

sdkmanager="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
if [ ! -x "$sdkmanager" ]; then
    echo "==> Downloading the Android command-line tools into $ANDROID_HOME"
    repo="https://dl.google.com/android/repository"
    # Pick the newest Linux build listed in Google's SDK repository.
    zip="$(curl -fsSL "$repo/repository2-3.xml" |
        grep -o 'commandlinetools-linux-[0-9]*_latest\.zip' | sort -V | tail -n 1 || true)"
    zip="${zip:-commandlinetools-linux-13114758_latest.zip}"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    curl -fL --progress-bar -o "$tmp/tools.zip" "$repo/$zip"
    unzip -q "$tmp/tools.zip" -d "$tmp"
    mkdir -p "$ANDROID_HOME/cmdline-tools"
    rm -rf "$ANDROID_HOME/cmdline-tools/latest"
    mv "$tmp/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"
fi

echo "==> Accepting the Android SDK licenses"
(yes || true) | "$sdkmanager" --sdk_root="$ANDROID_HOME" --licenses >/dev/null

echo "==> Installing $PLATFORM, $BUILD_TOOLS, $NDK and platform-tools (a few GB)"
"$sdkmanager" --sdk_root="$ANDROID_HOME" --install "$PLATFORM" "$BUILD_TOOLS" "$NDK" platform-tools

echo "==> Installing the Rust Android targets and cargo-apk"
rustup target add aarch64-linux-android x86_64-linux-android
if ! command -v cargo-apk >/dev/null; then
    cargo install cargo-apk --locked
fi

echo
echo "Done. Build the APK with:  scripts/android/build-apk.sh"
if [ "$ANDROID_HOME" != "$HOME/Android/Sdk" ]; then
    echo "Keep ANDROID_HOME=$ANDROID_HOME set when you build."
fi
echo "adb (for installing over USB or Wi-Fi) is $ANDROID_HOME/platform-tools/adb"

#!/usr/bin/env bash
# Builds the release APK: one universal file (arm64-v8a + x86_64), native code
# aligned for 16 KB memory pages, signed, and checked with check-apk.sh.
#
# Needs: ANDROID_HOME (SDK with platforms;android-36 and build-tools),
# ANDROID_NDK_ROOT (NDK), a JDK, `cargo install cargo-apk` and the Rust targets
# aarch64-linux-android and x86_64-linux-android.
#
# Signing: set CARGO_APK_RELEASE_KEYSTORE and CARGO_APK_RELEASE_KEYSTORE_PASSWORD.
# Without them a throwaway key is generated. That's fine for testing, but
# Android only installs an update over an app signed with the same key.
#
# Usage: scripts/android/build-apk.sh [OUTPUT.apk]
set -euo pipefail
cd "$(dirname "$0")/../.."

: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
: "${ANDROID_NDK_ROOT:?set ANDROID_NDK_ROOT to the Android NDK}"
output="${1:-target/release/apk/DiskTree.apk}"

# Android 15+ devices may use 16 KB memory pages, and Google Play requires
# native libraries built for them. cargo-apk ignores .cargo/config.toml
# rustflags but keeps RUSTFLAGS.
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-Clink-arg=-Wl,-z,max-page-size=16384"

if [ -z "${CARGO_APK_RELEASE_KEYSTORE:-}" ]; then
    echo "warning: no release keystore set; signing with a throwaway key" >&2
    keystore="$PWD/target/throwaway-release.keystore"
    if [ ! -f "$keystore" ]; then
        mkdir -p target
        keytool -genkeypair -keystore "$keystore" -alias release -keyalg RSA -keysize 2048 \
            -validity 10000 -storepass android -keypass android \
            -dname "CN=DiskTree unofficial build" >/dev/null
    fi
    export CARGO_APK_RELEASE_KEYSTORE="$keystore"
    export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=android
fi

# cargo-apk runs apksigner from the APK's folder, so the path must be absolute.
keystore_dir="$(cd "$(dirname "$CARGO_APK_RELEASE_KEYSTORE")" && pwd)"
export CARGO_APK_RELEASE_KEYSTORE="$keystore_dir/$(basename "$CARGO_APK_RELEASE_KEYSTORE")"

# No --target: cargo-apk builds every entry of build_targets into one APK.
cargo apk build -p disktree-app --lib --release

built="target/release/apk/DiskTree.apk"
if [ "$built" != "$output" ]; then
    mkdir -p "$(dirname "$output")"
    cp "$built" "$output"
fi
"$(dirname "$0")/check-apk.sh" "$output"

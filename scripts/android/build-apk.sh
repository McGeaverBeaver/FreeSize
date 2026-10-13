#!/usr/bin/env bash
# Builds the release APK: one universal file (arm64-v8a + x86_64), native code
# aligned for 16 KB memory pages, signed, and checked with check-apk.sh.
#
# Needs the Android SDK and NDK, a JDK, cargo-apk and the Rust Android
# targets; scripts/android/setup-linux.sh installs them all. The SDK is looked
# up in ANDROID_HOME, then ~/Android/Sdk.
#
# Signing: Android only installs an update over an app signed with the same
# key. The first build creates a key in ~/.android/disktree-release.keystore
# (password next to it in disktree-release.keystore.password) and every later
# build reuses it. Back both files up. To sign with another key, set
# CARGO_APK_RELEASE_KEYSTORE and CARGO_APK_RELEASE_KEYSTORE_PASSWORD.
#
# Usage: scripts/android/build-apk.sh [OUTPUT.apk]
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck source=scripts/android/env.sh
source scripts/android/env.sh

target="${CARGO_TARGET_DIR:-target}"
output="${1:-$target/release/apk/FreeSize.apk}"

# Android 15+ devices may use 16 KB memory pages, and Google Play requires
# native libraries built for them. cargo-apk ignores .cargo/config.toml
# rustflags but keeps RUSTFLAGS.
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-Clink-arg=-Wl,-z,max-page-size=16384"

if [ -z "${CARGO_APK_RELEASE_KEYSTORE:-}" ]; then
    key_dir="${DISKTREE_KEY_DIR:-$HOME/.android}"
    keystore="$key_dir/disktree-release.keystore"
    password_file="$keystore.password"
    if [ ! -f "$keystore" ]; then
        mkdir -p "$key_dir"
        (umask 077 && head -c 24 /dev/urandom | base64 | tr -d '/+=\n' >"$password_file")
        DISKTREE_KEY_PASSWORD="$(cat "$password_file")" keytool -genkeypair \
            -keystore "$keystore" -alias release -keyalg RSA -keysize 4096 -validity 10000 \
            -storepass:env DISKTREE_KEY_PASSWORD -keypass:env DISKTREE_KEY_PASSWORD \
            -dname "CN=FreeSize" >/dev/null
        chmod 600 "$keystore"
        echo "Created the signing key $keystore" >&2
        echo "and its password file $password_file. Back up both: Android only" >&2
        echo "installs updates signed with the same key." >&2
    fi
    if [ ! -f "$password_file" ]; then
        echo "error: $keystore exists but $password_file is missing" >&2
        exit 1
    fi
    export CARGO_APK_RELEASE_KEYSTORE="$keystore"
    CARGO_APK_RELEASE_KEYSTORE_PASSWORD="$(cat "$password_file")"
    export CARGO_APK_RELEASE_KEYSTORE_PASSWORD
fi

# cargo-apk runs apksigner from the APK's folder, so the path must be absolute.
keystore_dir="$(cd "$(dirname "$CARGO_APK_RELEASE_KEYSTORE")" && pwd)"
CARGO_APK_RELEASE_KEYSTORE="$keystore_dir/$(basename "$CARGO_APK_RELEASE_KEYSTORE")"
export CARGO_APK_RELEASE_KEYSTORE

# No --target: cargo-apk builds every entry of build_targets into one APK.
cargo apk build -p freesize-app --lib --release

built="$target/release/apk/FreeSize.apk"
if [ "$built" != "$output" ]; then
    mkdir -p "$(dirname "$output")"
    cp "$built" "$output"
fi
scripts/android/check-apk.sh "$output"

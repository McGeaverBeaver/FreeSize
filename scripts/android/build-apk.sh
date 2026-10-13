#!/usr/bin/env bash
# Builds the release APK: one universal file (arm64-v8a + x86_64), native code
# aligned for 16 KB memory pages, signed, and checked with check-apk.sh.
#
# cargo-apk builds the Rust side for both targets. Since 0.14 the APK also
# holds Java classes (crates/app/android/java: HubJob, the background job,
# and since 0.15.1 PairLink, which takes scanned pairing links),
# which cargo-apk can't add or declare, so the APK it produces is taken apart
# and put together again: its native libraries, plus classes.dex from javac
# and d8, the resources and the manifest in crates/app/android, then zipalign
# and apksigner with the same key.
#
# Needs the Android SDK (a platform and build-tools: aapt2, d8, zipalign,
# apksigner) and NDK, a JDK, cargo-apk and the Rust Android targets;
# scripts/android/setup-linux.sh installs them all. The SDK is looked up in
# ANDROID_HOME, then ~/Android/Sdk.
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

# The Java side, and the APK put together again around it.
build_tools="$ANDROID_HOME/build-tools/$(ls "$ANDROID_HOME/build-tools" | sort -V | tail -n 1)"
platform_jar="$(ls -d "$ANDROID_HOME"/platforms/android-*/android.jar | sort -V | tail -n 1)"
app="crates/app/android"
version_name="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
# The version code cargo-apk gave it, so Android keeps installing updates
# in order.
version_code="$("$build_tools/aapt2" dump badging "$built" | sed -n "s/.*versionCode='\([0-9]*\)'.*/\1/p" | head -n 1)"
if [ -z "$version_code" ] || [ -z "$version_name" ]; then
    echo "error: couldn't read the version from $built and Cargo.toml" >&2
    exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/classes" "$work/dex" "$work/lib"
# Java 11 bytecode: what d8 takes without desugaring surprises, on every
# JDK from 17.
javac -source 11 -target 11 -Xlint:-options -encoding UTF-8 \
    -classpath "$platform_jar" -d "$work/classes" \
    $(find "$app/java" -name '*.java')
"$build_tools/d8" --release --min-api 26 --lib "$platform_jar" \
    --output "$work/dex" $(find "$work/classes" -name '*.class')
"$build_tools/aapt2" compile --dir "$app/res" -o "$work/res.zip"
"$build_tools/aapt2" link -o "$work/base.apk" --manifest "$app/AndroidManifest.xml" \
    -I "$platform_jar" --version-code "$version_code" --version-name "$version_name" \
    --min-sdk-version 26 --target-sdk-version 36 "$work/res.zip"
unzip -q -o "$built" 'lib/*' -d "$work/lib"
cp "$work/base.apk" "$work/unsigned.apk"
(cd "$work/dex" && zip -q "$work/unsigned.apk" classes.dex)
# Native libraries stored, not compressed (extractNativeLibs is false), so
# Android maps them from the APK; zipalign puts them on 16 KB boundaries.
(cd "$work/lib" && zip -q -0 -r "$work/unsigned.apk" lib)
"$build_tools/zipalign" -P 16 -f 4 "$work/unsigned.apk" "$work/aligned.apk"
mkdir -p "$(dirname "$output")"
"$build_tools/apksigner" sign --ks "$CARGO_APK_RELEASE_KEYSTORE" \
    --ks-pass env:CARGO_APK_RELEASE_KEYSTORE_PASSWORD \
    --out "$output" "$work/aligned.apk"
rm -f "$output.idsig"
scripts/android/check-apk.sh "$output"

#!/usr/bin/env bash
# Checks that an APK will install and run on current Android devices:
# required ABIs, SDK levels, exported launcher activity, a valid v2+
# signature, and native libraries aligned for 16 KB pages.
#
# Usage: scripts/android/check-apk.sh APP.apk
set -euo pipefail

apk="${1:?usage: check-apk.sh APP.apk}"
: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
: "${ANDROID_NDK_ROOT:?set ANDROID_NDK_ROOT to the Android NDK}"

build_tools="$ANDROID_HOME/build-tools/$(ls "$ANDROID_HOME/build-tools" | sort -V | tail -n 1)"
readelf="$(ls "$ANDROID_NDK_ROOT"/toolchains/llvm/prebuilt/*/bin/llvm-readelf | head -n 1)"
failures=0
fail() {
    echo "FAIL: $*"
    if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::error::$*"; fi
    failures=$((failures + 1))
}
pass() { echo "ok:   $*"; }

echo "Checking $apk ($(du -h "$apk" | cut -f1))"
echo "Build tools: ${build_tools##*/}, NDK: $(sed -n 's/^Pkg.Revision *= *//p' "$ANDROID_NDK_ROOT/source.properties")"
badging="$("$build_tools/aapt2" dump badging "$apk")"
grep -E "^(package|sdkVersion|targetSdkVersion|native-code|launchable-activity|application-label):" <<<"$badging" || true

# Every phone sold in years is arm64; x86_64 covers emulators and Chromebooks.
abis="$(sed -n "s/^native-code: //p" <<<"$badging")"
for abi in arm64-v8a x86_64; do
    if grep -q "'$abi'" <<<"$abis"; then pass "contains $abi code"; else fail "no $abi native code"; fi
done

manifest="$("$build_tools/aapt2" dump xmltree --file AndroidManifest.xml "$apk")"
grep -A3 "E: uses-sdk" <<<"$manifest" || true
# Read SDK levels from the manifest itself; badging's field names vary between
# build-tools versions.
sdk_attr() {
    sed -n "s/.*android:$1([^)]*)=\([0-9][0-9]*\).*/\1/p" <<<"$manifest" | head -n 1
}
min_sdk="$(sdk_attr minSdkVersion)"
target_sdk="$(sdk_attr targetSdkVersion)"
if [ "${min_sdk:-0}" -ge 26 ]; then pass "minSdk $min_sdk"; else fail "minSdk is ${min_sdk:-missing}"; fi
# Android 14+ refuses targetSdk < 23; Google Play wants a recent one.
if [ "${target_sdk:-0}" -ge 35 ]; then pass "targetSdk $target_sdk"; else fail "targetSdk ${target_sdk:-missing} is too old"; fi

if grep -q "^launchable-activity:" <<<"$badging"; then pass "has a launcher activity"; else fail "no launcher activity"; fi
if grep -q "^application-debuggable" <<<"$badging"; then fail "release APK is debuggable"; else pass "not debuggable"; fi

# Android 12+ refuses to install apps whose launcher activity doesn't say exported.
if grep -Eq 'exported\(0x01010010\)=(true|\(type 0x12\)0xffffffff)' <<<"$manifest"; then
    pass "launcher activity is exported"
else
    fail "activity is not marked android:exported=true"
fi

signature="$("$build_tools/apksigner" verify --verbose "$apk")" || fail "apksigner verify failed"
if grep -Eq "Verified using v(2|3) scheme \(APK Signature Scheme v(2|3)\): true" <<<"$signature"; then
    pass "signed with APK Signature Scheme v2/v3"
else
    echo "$signature"
    fail "no v2/v3 signature"
fi

# 16 KB pages: every LOAD segment of every library must be aligned to 0x4000.
libs="$(mktemp -d)"
trap 'rm -rf "$libs"' EXIT
unzip -q -o "$apk" 'lib/*' -d "$libs"
for so in "$libs"/lib/*/*.so; do
    name="${so#"$libs"/}"
    worst=""
    for align in $("$readelf" -lW "$so" | awk '$1 == "LOAD" { print $NF }'); do
        if [ -z "$worst" ] || [ $((align)) -lt $((worst)) ]; then worst="$align"; fi
    done
    if [ -n "$worst" ] && [ $((worst)) -ge $((0x4000)) ]; then
        pass "$name LOAD segments aligned to $worst"
    else
        fail "$name is not 16 KB aligned (LOAD alignment $worst)"
    fi
done
# Uncompressed libraries must also sit on 16 KB boundaries inside the zip.
if "$build_tools/zipalign" -c -P 16 4 "$apk" >/dev/null 2>&1; then
    pass "zip entries aligned for 16 KB pages"
else
    fail "zipalign -c -P 16 failed"
fi

if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "All APK checks passed"

# shellcheck shell=bash
# Sourced by the Android scripts. Finds the SDK and NDK when ANDROID_HOME and
# ANDROID_NDK_ROOT aren't set, using the layout setup-linux.sh creates
# (~/Android/Sdk, the same place Android Studio uses on Linux).

export ANDROID_HOME="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"
if [ -z "${ANDROID_NDK_ROOT:-}" ]; then
    # The newest installed NDK.
    ANDROID_NDK_ROOT="$(ls -d "$ANDROID_HOME"/ndk/*/ 2>/dev/null | sort -V | tail -n 1 || true)"
    export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT%/}"
fi
if [ ! -d "$ANDROID_HOME/platforms" ] || [ ! -d "$ANDROID_NDK_ROOT" ]; then
    echo "error: no Android SDK and NDK in ${ANDROID_HOME}." >&2
    echo "Run scripts/android/setup-linux.sh, or set ANDROID_HOME and ANDROID_NDK_ROOT." >&2
    exit 1
fi

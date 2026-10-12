#!/usr/bin/env bash
# Builds a release on this machine and publishes it on GitHub.
#
# Builds, from the commit that's checked out:
#   disktree-android.apk               signed with your key (see android/build-apk.sh)
#   disktree-linux-<arch>.tar.gz       desktop app and command line (with the
#                                      agent's systemd unit)
#   disktree-hub-linux-<arch>.tar.gz   the Hub, with its systemd unit
#   disktree-windows-x86_64.zip        app and command line for Windows
#   disktree.plg, disktree-<version>-x86_64-1.txz
#                                      the Unraid plugin (a static build of
#                                      the command line, the page under
#                                      Tools); Unraid installs it from the
#                                      .plg in the latest release
#   disktree-*-linux-aarch64.tar.gz    Hub and command line for ARM boards, when
#                                      the aarch64 cross-compiler is installed
#   SHA256SUMS.txt
# runs the tests, then creates the GitHub release v<version> for that commit,
# with the files attached and release notes listing the changes. The version
# comes from the root Cargo.toml. macOS builds need a Mac, so there are none.
#
# With DISKTREE_IMAGE set (e.g. ghcr.io/mcgeaverbeaver/disktree-hub), it also
# builds the Hub's Docker image for amd64 and arm64 and pushes it as
# <version> and latest. That needs Docker with buildx, logged in to the
# registry (for GitHub: echo $TOKEN | docker login ghcr.io -u <user>
# --password-stdin, with a token that may write packages).
#
# Needs everything android/build-apk.sh needs, the MinGW cross-compiler and
# zip for the Windows build, musl-tools for the Unraid plugin's static build,
# and the GitHub CLI logged in (`gh auth login`).
#
# Usage: scripts/release.sh [--draft] [--build-only]
#   --draft       publish as a draft, to check before making it public
#   --build-only  build everything into dist/ without publishing
set -euo pipefail
cd "$(dirname "$0")/.."

draft=()
publish=true
for arg in "$@"; do
    case "$arg" in
        --draft) draft=(--draft) ;;
        --build-only) publish=false ;;
        *)
            echo "usage: scripts/release.sh [--draft] [--build-only]" >&2
            exit 2
            ;;
    esac
done

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
target="${CARGO_TARGET_DIR:-target}"
tag="v$version"
commit="$(git rev-parse HEAD)"

for tool in x86_64-w64-mingw32-gcc:mingw-w64 zip:zip musl-gcc:musl-tools file:file; do
    if ! command -v "${tool%%:*}" >/dev/null; then
        echo "error: ${tool%%:*} is missing; run: sudo apt install ${tool#*:}" >&2
        exit 1
    fi
done

image="${DISKTREE_IMAGE:-}"
if [ -n "$image" ] && $publish; then
    if ! docker buildx version >/dev/null 2>&1; then
        echo "error: DISKTREE_IMAGE is set but Docker buildx isn't available" >&2
        exit 1
    fi
fi

if $publish; then
    if ! command -v gh >/dev/null || ! gh auth status >/dev/null 2>&1; then
        echo "error: the GitHub CLI isn't logged in; run: sudo apt install gh && gh auth login" >&2
        exit 1
    fi
    # Publish exactly what's on GitHub, so the release matches its tag.
    if [ -n "$(git status --porcelain)" ]; then
        echo "error: there are uncommitted changes; commit or discard them first" >&2
        exit 1
    fi
    git fetch --quiet origin
    branch="$(git rev-parse --abbrev-ref HEAD)"
    if [ "$commit" != "$(git rev-parse "origin/$branch")" ]; then
        echo "error: this checkout isn't the same as origin/$branch; run git pull" >&2
        exit 1
    fi
    if gh release view "$tag" >/dev/null 2>&1; then
        echo "error: release $tag already exists; bump version in Cargo.toml first" >&2
        exit 1
    fi
fi

# Builds for four targets plus the Android NDK need room. A full disk
# shows up as a "Bus error" or "No space left on device" while linking.
free_gb="$(df -P -BG . | awk 'NR == 2 { sub("G", "", $4); print $4 }')"
if [ "${free_gb:-0}" -lt 15 ]; then
    echo "warning: only ${free_gb} GB free here; a release build needs about 15 GB." >&2
    echo "         If it fails with \"Bus error\" or \"No space left\", see \"Disk space\" in the README." >&2
fi

dist="dist/$tag"
rm -rf "$dist"
mkdir -p "$dist"
echo "==> Building DiskTree $version ($(git rev-parse --short HEAD)) into $dist"

echo "==> Tests"
# In release mode, so the tests reuse the release build below instead of
# compiling everything a second time (a debug build takes several GB).
cargo test --workspace --release --quiet

echo "==> Android"
scripts/android/build-apk.sh "$dist/disktree-android.apk"

package() {
    local name="$1" dir="$2"
    shift 2
    local stage
    stage="$(mktemp -d)"
    cp "$@" "$stage/"
    case "$name" in
        *.zip) (cd "$stage" && zip -q -9 "$OLDPWD/$dist/$name" ./*) ;;
        *) tar -czf "$dist/$name" -C "$stage" . ;;
    esac
    rm -rf "$stage"
    echo "    $dir -> $dist/$name"
}

echo "==> Linux"
cargo build --release --quiet -p disktree-app -p disktree-cli -p disktree-hub
units=packaging/systemd
package "disktree-linux-$(uname -m).tar.gz" "$target/release" \
    "$target/release/disktree-gui" "$target/release/disktree" \
    "$units/disktree-agent.service" "$units/disktree-agent.env"
package "disktree-hub-linux-$(uname -m).tar.gz" "$target/release" \
    "$target/release/disktree-hub" "$units/disktree-hub.service" "$units/disktree-hub.env"

if [ "$(uname -m)" != aarch64 ]; then
    if command -v aarch64-linux-gnu-gcc >/dev/null; then
        echo "==> Linux on ARM (Hub and command line)"
        rustup target add aarch64-unknown-linux-gnu >/dev/null 2>&1
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
            CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
            AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
            cargo build --release --quiet --target aarch64-unknown-linux-gnu \
            -p disktree-cli -p disktree-hub
        arm="$target/aarch64-unknown-linux-gnu/release"
        package disktree-cli-linux-aarch64.tar.gz "$arm" \
            "$arm/disktree" "$units/disktree-agent.service" "$units/disktree-agent.env"
        package disktree-hub-linux-aarch64.tar.gz "$arm" \
            "$arm/disktree-hub" "$units/disktree-hub.service" "$units/disktree-hub.env"
    else
        echo "    (no ARM builds: sudo apt install gcc-aarch64-linux-gnu to add them)"
    fi
fi

echo "==> Unraid plugin"
# Unraid has its own glibc; a static musl build runs on every version.
rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1
cargo build --release --quiet --target x86_64-unknown-linux-musl -p disktree-cli
scripts/unraid-plugin.sh "$target/x86_64-unknown-linux-musl/release/disktree" "$dist"

echo "==> Windows"
rustup target add x86_64-pc-windows-gnu >/dev/null 2>&1
cargo build --release --quiet --target x86_64-pc-windows-gnu -p disktree-app -p disktree-cli
win="$target/x86_64-pc-windows-gnu/release"
package disktree-windows-x86_64.zip "$win" "$win/disktree-gui.exe" "$win/disktree.exe"

(cd "$dist" && sha256sum -- * >SHA256SUMS.txt)
echo "==> Built:"
ls -lh "$dist"

if ! $publish; then
    echo "Not published (--build-only)."
    exit 0
fi

if [ -n "$image" ]; then
    echo "==> Docker image $image:$version (amd64, arm64)"
    docker buildx build --platform linux/amd64,linux/arm64 \
        --label "org.opencontainers.image.version=$version" \
        --label "org.opencontainers.image.revision=$commit" \
        -t "$image:$version" -t "$image:latest" --push .
fi

echo "==> Publishing $tag on GitHub"
gh release create "$tag" "$dist"/* \
    --target "$commit" \
    --title "DiskTree $version" \
    --generate-notes \
    "${draft[@]}"

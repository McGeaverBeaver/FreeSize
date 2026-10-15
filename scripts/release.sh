#!/usr/bin/env bash
# Builds a release on this machine and publishes it on GitHub.
#
# Builds, from the commit that's checked out:
#   freesize-android.apk               signed with your key (see android/build-apk.sh)
#   freesize-linux-<arch>.tar.gz       desktop app and command line (with the
#                                      agent's systemd unit)
#   freesize-hub-linux-<arch>.tar.gz   the Hub, with its systemd unit
#   freesize-windows-x86_64.zip        app and command line for Windows
#   freesize.plg, freesize-<version>-x86_64-1.txz
#                                      the Unraid plugin (a static build of
#                                      the command line, the page under
#                                      Tools); Unraid installs it from the
#                                      .plg in the latest release
#   freesize-nextcloud-<version>.tar.gz
#                                      the FreeSize Nextcloud app, to unpack
#                                      into Nextcloud's custom_apps folder
#   freesize-*-linux-aarch64.tar.gz    Hub and command line for ARM boards, when
#                                      the aarch64 cross-compiler is installed
#   SHA256SUMS.txt
#   the Hub's Docker image             for amd64 and arm64 (see below)
# runs the tests, then creates the GitHub release v<version> for that commit,
# with the files attached and release notes listing the changes. The version
# comes from the root Cargo.toml. macOS builds need a Mac, so there are none.
#
# The Docker image is what a GitHub Actions workflow would publish, built
# here instead so it costs no Actions minutes: ghcr.io/<owner>/freesize-hub,
# a package of the GitHub repository, tagged <version> and latest (the tag
# Unraid's template pulls), drafts too: the owner publishes drafts on GitHub's
# website, which can't move the tag. It holds the Hub built
# above, for amd64 and arm64, rather than compiling it again inside Docker.
# The GitHub CLI's token logs in to the registry (it needs the
# write:packages scope) in a throwaway Docker config, so the token isn't
# left on disk. FREESIZE_IMAGE names another image (log in to its registry
# yourself); --no-image leaves the image out.
#
# Needs everything android/build-apk.sh needs, the MinGW cross-compiler and
# zip for the Windows build, musl-tools for the Unraid plugin's static build,
# for the image the aarch64 cross-compiler and Docker with buildx, and the
# GitHub CLI logged in (`gh auth login`).
#
# Usage: scripts/release.sh [--draft] [--build-only] [--no-image]
#        scripts/release.sh --publish [--no-image]
#   --draft       publish as a draft, to check before making it public; the
#                 image is pushed and tagged latest all the same
#   --build-only  build everything (the image too) without publishing
#   --no-image    leave out the Docker image
#   --publish     make this version's draft public (if it isn't already)
#                 and point the image's latest tag at it; builds nothing
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
    echo "usage: scripts/release.sh [--draft] [--build-only] [--no-image]" >&2
    echo "       scripts/release.sh --publish [--no-image]" >&2
    exit 2
}

draft=()
publish=true
with_image=true
promote=false
for arg in "$@"; do
    case "$arg" in
        --draft) draft=(--draft) ;;
        --build-only) publish=false ;;
        --no-image) with_image=false ;;
        --publish) promote=true ;;
        *) usage ;;
    esac
done
if $promote && { [ ${#draft[@]} -gt 0 ] || ! $publish; }; then
    usage
fi

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
target="${CARGO_TARGET_DIR:-target}"
tag="v$version"
commit="$(git rev-parse HEAD)"
# "Owner/Repo" on GitHub, from the origin remote.
repo="$(git remote get-url origin | sed -nE 's#^.*github\.com[:/]([^/]+/[^/]+)$#\1#p')"
repo="${repo%.git}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

image=""
if $with_image; then
    image="${FREESIZE_IMAGE:-}"
    if [ -z "$image" ]; then
        if [ -z "$repo" ]; then
            echo "error: origin isn't on GitHub; set FREESIZE_IMAGE or pass --no-image" >&2
            exit 1
        fi
        owner="${repo%%/*}"
        image="ghcr.io/${owner,,}/freesize-hub"
    fi
    if ! docker buildx version >/dev/null 2>&1; then
        echo "error: the Docker image needs Docker with buildx; run:" >&2
        echo "       sudo apt install docker.io docker-buildx && sudo usermod -aG docker $USER" >&2
        echo "       and log in again (or pass --no-image)" >&2
        exit 1
    fi
    if ! docker info >/dev/null 2>&1; then
        echo "error: can't reach Docker; is $USER in the docker group? Log in again after adding" >&2
        echo "       it, or run this with: sg docker -c 'scripts/release.sh ...'" >&2
        exit 1
    fi
    # The builder is kept with the usual Docker settings, also while
    # DOCKER_CONFIG points at the throwaway config below.
    export BUILDX_CONFIG="${BUILDX_CONFIG:-${DOCKER_CONFIG:-$HOME/.docker}/buildx}"
fi

# Logs in to GitHub's registry with the GitHub CLI's token, in a throwaway
# Docker config so the token isn't left on disk. Other registries use your
# own `docker login`.
registry_login() {
    case "$image" in
        ghcr.io/*) ;;
        *) return 0 ;;
    esac
    # Classic tokens list their scopes; fine-grained ones don't.
    local scopes
    scopes="$(gh api -i user 2>/dev/null | tr -d '\r' | sed -n 's/^[Xx]-[Oo][Aa]uth-[Ss]copes: //p')"
    if [ -n "$scopes" ] && [[ "$scopes" != *write:packages* ]]; then
        echo "error: the GitHub CLI's token can't push packages; run: gh auth refresh -s write:packages" >&2
        exit 1
    fi
    export DOCKER_CONFIG="$work/docker"
    mkdir -p "$DOCKER_CONFIG"
    if [ -d "$HOME/.docker/cli-plugins" ]; then
        ln -s "$HOME/.docker/cli-plugins" "$DOCKER_CONFIG/cli-plugins"
    fi
    gh auth token | docker login ghcr.io -u "$(gh api user -q .login)" --password-stdin >/dev/null
}

# Points the image's latest tag at this version, in the registry. (Until
# 0.16.2 it also tagged the old disktree-hub name; that and disktree.plg
# stopped at the owner's word, 2026-10-13.)
tag_latest() {
    echo "==> Pointing $image:latest at $version"
    docker buildx imagetools create --tag "$image:latest" "$image:$version"
}

# Unraid pulls without logging in, so the package has to be public; GitHub
# only lets that be changed on its website.
check_visibility() {
    case "$image" in
        ghcr.io/*) ;;
        *) return 0 ;;
    esac
    local path="${image#ghcr.io/}"
    local owner="${path%%/*}" name="${path#*/}"
    local visibility
    visibility="$(gh api "users/$owner/packages/container/$name" -q .visibility 2>/dev/null ||
        gh api "orgs/$owner/packages/container/$name" -q .visibility 2>/dev/null || true)"
    if [ "$visibility" = private ]; then
        echo "note: the image is private, so Unraid and Docker can't pull it without logging in." >&2
        echo "      Make it public once: https://github.com/users/$owner/packages/container/$name/settings" >&2
        echo "      (Danger Zone -> Change visibility)." >&2
    fi
}

for tool in x86_64-w64-mingw32-gcc:mingw-w64 zip:zip musl-gcc:musl-tools file:file; do
    if $promote; then
        break
    fi
    if ! command -v "${tool%%:*}" >/dev/null; then
        echo "error: ${tool%%:*} is missing; run: sudo apt install ${tool#*:}" >&2
        exit 1
    fi
done
if [ -n "$image" ] && ! $promote; then
    if [ "$(uname -m)" != x86_64 ]; then
        echo "error: the Docker image is built on x86_64 machines; pass --no-image" >&2
        exit 1
    fi
    if ! command -v aarch64-linux-gnu-gcc >/dev/null; then
        echo "error: the Docker image needs the Hub for arm64 too; run:" >&2
        echo "       sudo apt install gcc-aarch64-linux-gnu (or pass --no-image)" >&2
        exit 1
    fi
fi

if $publish; then
    if ! command -v gh >/dev/null || ! gh auth status >/dev/null 2>&1; then
        echo "error: the GitHub CLI isn't logged in; run: sudo apt install gh && gh auth login" >&2
        exit 1
    fi
fi

if $promote; then
    is_draft="$(gh release view "$tag" --json isDraft -q .isDraft 2>/dev/null || true)"
    if [ -z "$is_draft" ]; then
        echo "error: there's no release $tag to publish" >&2
        exit 1
    fi
    if [ -n "$image" ]; then
        registry_login
        if ! docker buildx imagetools inspect "$image:$version" >/dev/null 2>&1; then
            echo "error: $image:$version isn't in the registry; pass --no-image to publish without it" >&2
            exit 1
        fi
        tag_latest
    fi
    if [ "$is_draft" = true ]; then
        echo "==> Publishing $tag on GitHub"
        gh release edit "$tag" --draft=false --latest
    else
        echo "    $tag is public already"
    fi
    if [ -n "$image" ]; then
        check_visibility
    fi
    exit 0
fi

if $publish; then
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
    echo "         If it fails with \"Bus error\" or \"No space left\", see \"Disk space\" in docs/DEVELOPING.md." >&2
fi

dist="dist/$tag"
rm -rf "$dist"
mkdir -p "$dist"
echo "==> Building FreeSize $version ($(git rev-parse --short HEAD)) into $dist"

echo "==> Tests"
# In release mode, so the tests reuse the release build below instead of
# compiling everything a second time (a debug build takes several GB).
cargo test --workspace --release --quiet

echo "==> Android"
scripts/android/build-apk.sh "$dist/freesize-android.apk"

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
cargo build --release --quiet -p freesize-app -p freesize-cli -p freesize-hub
units=packaging/systemd
package "freesize-linux-$(uname -m).tar.gz" "$target/release" \
    "$target/release/freesize-gui" "$target/release/freesize" \
    "$units/freesize-agent.service" "$units/freesize-agent.env"
package "freesize-hub-linux-$(uname -m).tar.gz" "$target/release" \
    "$target/release/freesize-hub" "$units/freesize-hub.service" "$units/freesize-hub.env"

if [ "$(uname -m)" != aarch64 ]; then
    if command -v aarch64-linux-gnu-gcc >/dev/null; then
        echo "==> Linux on ARM (Hub and command line)"
        rustup target add aarch64-unknown-linux-gnu >/dev/null 2>&1
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
            CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
            AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
            cargo build --release --quiet --target aarch64-unknown-linux-gnu \
            -p freesize-cli -p freesize-hub
        arm="$target/aarch64-unknown-linux-gnu/release"
        package freesize-cli-linux-aarch64.tar.gz "$arm" \
            "$arm/freesize" "$units/freesize-agent.service" "$units/freesize-agent.env"
        package freesize-hub-linux-aarch64.tar.gz "$arm" \
            "$arm/freesize-hub" "$units/freesize-hub.service" "$units/freesize-hub.env"
    else
        echo "    (no ARM builds: sudo apt install gcc-aarch64-linux-gnu to add them)"
    fi
fi

echo "==> Unraid plugin"
# Unraid has its own glibc; a static musl build runs on every version.
rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1
cargo build --release --quiet --target x86_64-unknown-linux-musl -p freesize-cli
scripts/unraid-plugin.sh "$target/x86_64-unknown-linux-musl/release/freesize" "$dist"

echo "==> Windows"
rustup target add x86_64-pc-windows-gnu >/dev/null 2>&1
cargo build --release --quiet --target x86_64-pc-windows-gnu -p freesize-app -p freesize-cli
win="$target/x86_64-pc-windows-gnu/release"
package freesize-windows-x86_64.zip "$win" "$win/freesize-gui.exe" "$win/freesize.exe"

echo "==> Nextcloud app"
scripts/nextcloud-app.sh "$dist"

(cd "$dist" && sha256sum -- * >SHA256SUMS.txt)
# The release key signs the sums, so apps can check an update came from
# here (see release/README.md). Without the key the release is unsigned and
# apps won't install it themselves.
release_key="${FREESIZE_RELEASE_KEY:-$HOME/.freesize/release-key}"
if [ -f "$release_key" ]; then
    "$target/release/freesize" release sign "$dist/SHA256SUMS.txt" --key "$release_key"
    "$target/release/freesize" release verify "$dist/SHA256SUMS.txt" --public-key release/public-key.txt
else
    echo "warning: no release key at $release_key; the release is unsigned, so apps" >&2
    echo "         won't update to it themselves (see release/README.md)" >&2
fi
echo "==> Built:"
ls -lh "$dist"

if [ -n "$image" ]; then
    echo "==> Docker image $image:$version (amd64, arm64)"
    # The Hubs built above, as the Dockerfile's `hub` stage would have
    # them, so Docker compiles nothing: that needs a Rust image and several
    # GB in Docker's own storage.
    hub="$work/hub"
    mkdir -p "$hub/amd64" "$hub/arm64"
    cp "$target/release/freesize-hub" "$hub/amd64/"
    cp "$target/aarch64-unknown-linux-gnu/release/freesize-hub" "$hub/arm64/"
    cp /etc/ssl/certs/ca-certificates.crt "$hub/"
    # They link to this machine's glibc, so they mustn't need a newer one
    # than the image's Debian has.
    for bin in "$hub"/*/freesize-hub; do
        need="$(readelf -V "$bin" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -n 1)"
        if [ "$(printf '%s\n' "$need" 2.36 | sort -V | tail -n 1)" != 2.36 ]; then
            echo "error: $bin needs glibc $need, newer than the 2.36 in the image" >&2
            exit 1
        fi
    done
    # The Dockerfile's final image, with a `hub` stage taking those.
    {
        echo "# syntax=docker/dockerfile:1"
        echo "FROM scratch AS hub"
        echo "COPY --from=prebuilt / /"
        sed -n '/^# The final image/,$p' Dockerfile
    } >"$work/Dockerfile"
    # A builder of its own, as Actions' setup-buildx makes: the default one
    # can't always build for two platforms.
    docker buildx inspect freesize >/dev/null 2>&1 ||
        docker buildx create --name freesize --driver docker-container >/dev/null
    labels=(--label "org.opencontainers.image.version=$version"
        --label "org.opencontainers.image.revision=$commit")
    if [ -n "$repo" ]; then
        # Links the package to the repository on GitHub, which reads it from
        # the index of an image for several platforms, not their labels.
        labels+=(--label "org.opencontainers.image.source=https://github.com/$repo"
            --annotation "index:org.opencontainers.image.source=https://github.com/$repo")
    fi
    output=(--output type=cacheonly)
    if $publish; then
        registry_login
        output=(--push)
    fi
    # No provenance attestations: they show up as an extra "unknown/unknown"
    # platform next to amd64 and arm64.
    docker buildx build --builder freesize --platform linux/amd64,linux/arm64 \
        --file "$work/Dockerfile" --build-context prebuilt="$hub" \
        --provenance=false "${labels[@]}" --tag "$image:$version" "${output[@]}" .
fi

if ! $publish; then
    echo "Not published (--build-only)."
    exit 0
fi

echo "==> Publishing $tag on GitHub"
gh release create "$tag" "$dist"/* \
    --target "$commit" \
    --title "FreeSize $version" \
    --generate-notes \
    "${draft[@]}"

if [ -n "$image" ]; then
    tag_latest
    check_visibility
fi

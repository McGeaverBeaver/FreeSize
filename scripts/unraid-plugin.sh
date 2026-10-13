#!/usr/bin/env bash
# Builds the Unraid plugin from a static x86_64 `freesize` binary:
#   <out>/freesize-<version>-x86_64-1.txz   the Slackware package
#   <out>/freesize.plg                       the plugin, pointing at the
#                                            package in the GitHub release
#   <out>/disktree.plg                       for servers with the plugin
#                                            from before the rename: an
#                                            update that installs FreeSize
# The version comes from the root Cargo.toml.
#
# Usage: scripts/unraid-plugin.sh <freesize binary> <out dir>
# (release.sh runs it with the musl build: cargo build --release
#  --target x86_64-unknown-linux-musl -p freesize-cli)
set -euo pipefail
cd "$(dirname "$0")/.."

if [ $# -ne 2 ]; then
    echo "usage: scripts/unraid-plugin.sh <freesize binary> <out dir>" >&2
    exit 2
fi
binary="$1"
out="$2"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
package="freesize-$version-x86_64-1"

# Unraid is Slackware with its own glibc: only a static binary runs on
# every version.
if ! file "$binary" | grep -q "static"; then
    echo "error: $binary isn't statically linked; build it for x86_64-unknown-linux-musl" >&2
    exit 1
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
cp -r unraid/plugin/root/. "$stage/"
install -D -m 755 "$binary" "$stage/usr/local/bin/freesize"
find "$stage" -type d -exec chmod 755 {} +
find "$stage" -type f -exec chmod 644 {} +
chmod 755 "$stage/usr/local/bin/freesize" "$stage/etc/rc.d/rc.freesize" \
    "$stage"/usr/local/emhttp/plugins/freesize/event/*

mkdir -p "$out"
tar --owner=0 --group=0 --numeric-owner --sort=name -C "$stage" -cJf "$out/$package.txz" .
sha="$(sha256sum "$out/$package.txz" | cut -d ' ' -f 1)"
sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$sha/g" unraid/plugin/freesize.plg >"$out/freesize.plg"
sed -e "s/@VERSION@/$version/g" unraid/plugin/disktree.plg >"$out/disktree.plg"
echo "    unraid/plugin -> $out/$package.txz, $out/freesize.plg, $out/disktree.plg"

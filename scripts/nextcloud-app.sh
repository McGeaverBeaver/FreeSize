#!/usr/bin/env bash
# Packs the FreeSize Nextcloud app (nextcloud/freesize) as
# <dist>/freesize-nextcloud-<version>.tar.gz, with the folder `freesize` at
# its top, the way Nextcloud's apps folder wants it:
#
#   scripts/nextcloud-app.sh dist/v0.20.0
#
# Installing by hand: unpack it into Nextcloud's custom_apps (or apps)
# folder and run `occ app:enable freesize`.
set -euo pipefail
cd "$(dirname "$0")/.."
dist="${1:?usage: scripts/nextcloud-app.sh DIST}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
app=nextcloud/freesize
listed="$(sed -n 's:.*<version>\(.*\)</version>.*:\1:p' "$app/appinfo/info.xml")"
sent="$(sed -n "s/.*const VERSION = '\(.*\)';/\1/p" "$app/lib/AppInfo/Application.php")"
if [ "$listed" != "$version" ] || [ "$sent" != "$version" ]; then
    echo "error: the Nextcloud app says $listed (info.xml) and $sent (Application.php), not $version" >&2
    exit 1
fi
if command -v php >/dev/null; then
    find "$app" -name '*.php' -print0 | xargs -0 -n1 php -l >/dev/null
    php "$app/tests/run.php" >/dev/null
fi
mkdir -p "$dist"
# The release key's public half goes in, so the app can check the releases
# it updates itself from (empty until the owner makes the key: then the app
# says to update by hand).
cp release/public-key.txt "$app/release-key.txt"
trap 'rm -f "$app/release-key.txt"' EXIT
tar -czf "$dist/freesize-nextcloud-$version.tar.gz" -C nextcloud \
    --exclude=freesize/tests --owner=0 --group=0 freesize
echo "    $app -> $dist/freesize-nextcloud-$version.tar.gz"

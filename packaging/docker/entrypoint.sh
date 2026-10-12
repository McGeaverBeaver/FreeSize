#!/bin/sh
# Starts the Hub as PUID:PGID. Started as root, it first gives the data
# folder to that user (a new Unraid appdata folder belongs to root); started
# with --user, it runs as that user as it is.
set -eu
data="${DISKTREE_HUB_DATA:-/data}"
if [ "$(id -u)" = 0 ]; then
    uid="${PUID:-99}"
    gid="${PGID:-100}"
    mkdir -p "$data"
    if [ "$(stat -c %u:%g "$data")" != "$uid:$gid" ]; then
        chown -R "$uid:$gid" "$data"
    fi
    exec setpriv --reuid="$uid" --regid="$gid" --clear-groups -- /usr/local/bin/disktree-hub "$@"
fi
exec /usr/local/bin/disktree-hub "$@"

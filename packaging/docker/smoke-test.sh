#!/bin/bash
# Checks a built Hub image the way a fresh Unraid install starts it: a data
# folder belonging to root, PUID/PGID 99:100 from the image, then again with
# other ids, with every capability dropped but the three the start script
# needs, and with a read-only root. Each time the Hub must turn healthy, run
# as the asked user with no capabilities, own its data folder and answer
# /api/status. Used by .github/workflows/hub-image.yml before it pushes.
#
# Usage: packaging/docker/smoke-test.sh IMAGE [PLATFORM]
set -Eeuo pipefail
image="$1"
platform="${2:-}"
platform_args=()
[ -n "$platform" ] && platform_args=(--platform "$platform")
work="$(mktemp -d)"
name="freesize-smoke-$$"
trap 'docker rm -f "$name" >/dev/null 2>&1 || true; sudo rm -rf "$work" 2>/dev/null || rm -rf "$work"' EXIT

check() {
    local label="$1" uid="$2" gid="$3"
    shift 3
    local data="$work/$label"
    mkdir -p "$data"
    docker run -d --name "$name" "${platform_args[@]}" --health-interval=2s --health-start-period=1s \
        -v "$data:/data" "$@" "$image" >/dev/null
    local status=""
    for _ in $(seq 1 90); do
        status="$(docker inspect -f '{{.State.Health.Status}}' "$name")"
        [ "$status" = healthy ] && break
        [ "$(docker inspect -f '{{.State.Running}}' "$name")" = true ] || break
        sleep 1
    done
    if [ "$status" != healthy ]; then
        echo "FAIL ($label): the Hub isn't healthy ($status)" >&2
        docker logs "$name" >&2 || true
        exit 1
    fi
    local proc
    proc="$(docker exec "$name" cat /proc/1/status)"
    grep -Eq "^Uid:\s+$uid\s+$uid\s+$uid\s+$uid$" <<<"$proc" || { echo "FAIL ($label): not running as uid $uid" >&2; echo "$proc" >&2; exit 1; }
    grep -Eq "^Gid:\s+$gid\s+$gid\s+$gid\s+$gid$" <<<"$proc" || { echo "FAIL ($label): not running as gid $gid" >&2; exit 1; }
    grep -Eq "^CapEff:\s+0+$" <<<"$proc" || { echo "FAIL ($label): the Hub kept capabilities" >&2; exit 1; }
    local owner
    owner="$(docker exec "$name" stat -c %u:%g /data/hub-key)"
    [ "$owner" = "$uid:$gid" ] || { echo "FAIL ($label): /data/hub-key belongs to $owner" >&2; exit 1; }
    docker exec "$name" freesize-hub health >/dev/null || { echo "FAIL ($label): health" >&2; exit 1; }
    docker stop -t 20 "$name" >/dev/null
    local code
    code="$(docker inspect -f '{{.State.ExitCode}}' "$name")"
    [ "$code" = 0 ] || { echo "FAIL ($label): stopped with exit code $code" >&2; exit 1; }
    docker rm "$name" >/dev/null
    echo "ok: $label"
}

check fresh 99 100
# The same folder again: the Hub starts on the data it made (an update).
check fresh 99 100
check puid 1000 1000 -e PUID=1000 -e PGID=1000
check capabilities 99 100 --cap-drop ALL --cap-add CHOWN --cap-add SETUID --cap-add SETGID
check read-only 99 100 --read-only --tmpfs /tmp

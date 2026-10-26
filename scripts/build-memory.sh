# Sourced by release.sh and android/build-apk.sh before they run cargo.
#
# Each rustc a release build runs can take 1.5 GB or more (codegen-units = 1
# and thin LTO), and cargo starts one per CPU. On a VM with little memory
# that ends in "Cannot allocate memory (os error 12)", often followed by
# "can't find crate for `std`". This picks the number of parallel jobs from
# the memory that's free, unless CARGO_BUILD_JOBS is set, and removes the
# half-written archives such a failure leaves in the target folder.
#
# Uses $target (the cargo target folder) from the script that sources it.

# Leftovers of an archive rustc was writing when it failed.
find "$target" -maxdepth 4 -type d -name '.tmp*.temp-archive' -prune \
    -exec rm -rf {} + 2>/dev/null || true

if [ -z "${CARGO_BUILD_JOBS:-}" ] && [ -r /proc/meminfo ]; then
    # Free memory plus free swap, in MB.
    free_mb="$(awk '/^(MemAvailable|SwapFree):/ { kb += $2 } END { print int(kb / 1024) }' /proc/meminfo)"
    cpus="$(nproc 2>/dev/null || echo 1)"
    jobs=$(( free_mb / 1536 ))
    [ "$jobs" -ge 1 ] || jobs=1
    if [ "$jobs" -lt "$cpus" ]; then
        export CARGO_BUILD_JOBS="$jobs"
        echo "note: $(( free_mb / 1024 )) GB of memory free (with swap): building $jobs at a time, not $cpus." >&2
        echo "      Set CARGO_BUILD_JOBS to choose; see \"Memory\" in docs/DEVELOPING.md." >&2
    fi
    unset free_mb cpus jobs
fi

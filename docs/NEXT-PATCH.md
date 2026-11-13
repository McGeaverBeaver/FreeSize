# Next patch

Small things the owner wants in the next update, whatever release that
turns out to be. Add to it as things come up; when a release ships what's
here, move those lines into docs/ROADMAP.md and clear them from this list.

## Add

## Fix

- Unraid plugin, Docker and VMs tab: volume names are cut to 12 characters
  (`docker.rs`, `short()` on the name), as if they were ids. Show named
  volumes in full; only shorten the 64-character anonymous ones.
- Unraid plugin, Docker and VMs tab: "Files no VM uses" also lists the
  disks and ISOs VMs do use (protected, no button) and counts them in its
  total. Show those apart, like Docker's folded "In use" group.
- Unraid plugin, Map tab: a map built while something is in the quarantine
  shows `.disktree-quarantine` as a folder (and likely `.freesize`, which
  holds the last map's file). Leave both out of the map.

## Do

# Testing a release

What to try by hand before a release goes public. The automated checks
(`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `cargo test --workspace`, the Unraid page's PHP test) run in
`scripts/release.sh` and in CI; this list is what they can't cover. Tick
what was tried in the release's draft notes, and say what wasn't.

## Every release

- [ ] The version in `Cargo.toml` is higher than the last release's.
- [ ] `SHA256SUMS.txt` and `SHA256SUMS.txt.sig` are in the release, and
      `freesize release verify SHA256SUMS.txt` passes with the public key
      in `release/public-key.txt`.
- [ ] The release notes say what changed, and what wasn't tested.

## Desktop app (Windows and Linux; macOS when someone has a Mac)

- [ ] Scans the home folder and a whole drive; totals look right; the
      status line says how many folders couldn't be read.
- [ ] Clean up tab: Folders FreeSize couldn't read names them; Scan as
      administrator (Windows) or Scan as root (Linux) asks, scans, and the
      result opens read-only with those folders now readable.
- [ ] Delete moves to the quarantine; Put back restores the file byte for
      byte (compare the size and a checksum); Empty frees the space.
- [ ] Clean junk: Find junk lists nothing the system's own tool would
      refuse (compare with Settings → Storage → Temporary files on
      Windows); a rule's paths open with Show; with Chrome open its cache
      says it's skipped; Clean frees what it said within a few percent
      (compare the drive's free space before and after) and the result
      names what was skipped; a "check first" rule lands in the
      quarantine and Put back restores it; `freesize junk` lists the same.
- [ ] Leftovers of removed programs: run Find junk, uninstall a program
      whose settings folder is in AppData or `~/.config` (Discord, say),
      set the folder's files two weeks back (or wait), and Find junk lists
      the folder under Apps with the program's name and the date; nothing
      of a program still installed is listed; Clean moves it to the
      quarantine and Put back restores it. On Linux a Flatpak removed
      without `--delete-data` shows up without the earlier look.
- [ ] Scanning a whole drive on btrfs (Linux) or APFS (macOS): the Clean
      up tab's "Not in any file" names the snapshots and, on a Mac, the
      purgeable space.
- [ ] With the Nextcloud client set up: `freesize nextcloud` lists the
      account and its sync folders, the sizes match the client's, and a
      file made online-only counts as not stored here.
- [ ] Opens a saved scan; Export writes one that opens again.
- [ ] Hub: pairs from a link, shows up online, a delete request is approved
      on the device and lands in the quarantine.
- [ ] Updates: with the check on, a newer release is offered; Update
      downloads, verifies, swaps the executable and starts the new version.
      An edited `SHA256SUMS.txt` is refused.

## Android

- [ ] Installs over the previous version (same signing key).
- [ ] All files access and Usage access are asked for and, once granted,
      the home screen shows apps and the system breakdown.
- [ ] A delete lands in the quarantine and Put back restores it.
- [ ] Free up space: Old downloads, Screen recordings and Empty folders
      show when there are any, say what they'd move, and land in the
      quarantine.
- [ ] Pairs from the QR code; a delete from the Hub is approved on the
      phone.
- [ ] In the background (0.14): with the app closed for a day, the Hub
      shows the phone seen within 15 minutes all day and its disks and
      quarantine up to date; a scan made while the Hub was unreachable
      arrives once it is; a request from the Hub, and an approval for a
      server the phone approves for, show as a notification within 15
      minutes, and tapping it opens the app on the request; the battery
      screen doesn't list FreeSize above the usual apps.
- [ ] Updates: a newer release is offered and opens for installing.

## Hub

- [ ] Starts fresh: makes its key and certificate, the dashboard signs in
      through Authentik over HTTPS.
- [ ] Upgrades from the previous version: devices, scans, alerts and the
      audit log are all there; `/api/audit/verify` passes.
- [ ] A device behind the Hub's version is flagged; the Hub says when a
      newer release exists (with the update check on).
- [ ] Alerts reach a channel; the message carries the audit log's head.
- [ ] A device's Junk tab shows what its cleaner found; Clean on a safe
      rule is approved on the device and the tab updates; a junk alert
      fires above the limit set on the Alerts page.
- [ ] A wrong pairing code ten times from one address is refused for a
      minute; other addresses still pair.
- [ ] Copies: Connect an account opens Nextcloud's sign-in page, the
      account appears with its files listed, and the app password shows
      in Nextcloud's security settings; for 20 files checked by hand on
      three devices, "kept on N devices" is right; a file kept on one
      device only, not yet uploaded, is in Only one copy; Disconnect
      removes the app password from Nextcloud.
- [ ] Copies, Suggestions (0.13): on a Windows PC with virtual files on,
      Free on a 10 GB folder is approved on the PC, the files stay listed,
      the space is free after the client's next sync, and Keep here
      brings them back; a phone's camera files the account already has
      are offered and land in the phone's quarantine; Move in on a folder
      on another drive copies, checks and quarantines the original, and
      the folder appears on Nextcloud; on Unraid, External storage adds a
      mounted share and it shows in Nextcloud's files.

## Unraid plugin

- [ ] Installs from the release's `freesize.plg` and updates from the
      previous version with its settings, pairing and quarantine intact.
- [ ] The page shows the array, shares, Docker's findings; a move between
      disks runs and the log is clean.
- [ ] Quarantine tab: a `.Recycle.bin` with files in it shows under Trash
      folders in the shares with its paths; Empty for good removes it and
      `freesize unraid junk list` agrees.
- [ ] Shares tab, Nextcloud: the per-user numbers match `du -sh` of
      `files`, `files_versions` and `files_trashbin` within 1%, and the
      retention lines match `config.php`.
- [ ] A stopped VM's sparse vdisk moves between disks with its holes kept
      (`du -h` of the copy stays well under `ls -l`'s size) and the VM
      starts from it; the move's row shows the file being copied.
- [ ] Empty a disk plans every share on the disk and runs the moves one
      after another; with the tick, the originals are gone once each move
      says every copy checked out.
- [ ] After a round on the whole array, `/var/log/freesize.log` gives the
      peak memory, and it stays well under the server's free memory.

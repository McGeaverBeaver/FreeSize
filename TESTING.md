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

- [ ] Changes tab after a few scans of the same folder: the chart shows a
      point per scan and this scan; clicking a point compares with it (the
      picker follows), and each listed folder has a bar; on a phone the
      chart fits and a tap compares.
- [ ] Review a folder: right-click an old backup folder of a few thousand
      files in the tree, Review this folder; the tabs' counts add up, Photos
      shows previews, Worth a look lists a `.kdbx`, a `.pst` and a tax
      file put there; Compare with my current files (after scanning the
      whole drive) matches files checked by hand, a file changed by one
      byte is Only here; Look inside files finds a word in a `.txt`, a
      `.docx`, an `.xlsx` and a PDF, and a word changed in Settings is used
      the next time. On a phone, highlight the folder and tap Review.
- [ ] `freesize review` on the Unraid server, on a share's old backup
      folder with `--in` the share: same counts as the app would show.
- [ ] Scans the home folder and a whole drive; totals look right; the
      status line says how many folders couldn't be read.
- [ ] Clean up tab: Folders FreeSize couldn't read names them; Scan as
      administrator (Windows) or Scan as root (Linux) asks, scans, and the
      result opens read-only with those folders now readable.
- [ ] Clean up tab: the cards show sizes; Find duplicates, then Review:
      Show EXE files and Select extra copies ticks all but the Keep copy of
      each EXE set and nothing else; ticking the Keep copy too flags the
      set and the confirmation; Review selection… lists exactly what's ticked, Keep
      takes one off, and Delete all moves the rest to the quarantine. Find
      similar photos shows the sets side by side; Select all but the
      largest, then Unselect all. On a phone the same, with the tiles two across
      and the tick boxes easy to hit.
- [ ] Delete moves to the quarantine; Put back restores the file byte for
      byte (compare the size and a checksum); Empty frees the space.
- [ ] Clean junk: Find junk lists nothing the system's own tool would
      refuse (compare with Settings → Storage → Temporary files on
      Windows); a rule's paths open with Show; with Chrome open its cache
      says it's skipped; Clean frees what it said within a few percent
      (compare the drive's free space before and after) and the result
      names what was skipped; a "check first" rule lands in the
      quarantine and Put back restores it; `freesize junk` lists the same.
- [ ] Clean junk (0.17.0): the rules for the system's tools sit at the
      bottom without tick boxes and aren't in the "Free X" total; Show
      command opens the command with Copy (paste it somewhere) and Run.
      On Windows without administrator rights, "Run as administrator" on
      the hibernation file shows UAC and FreeSize opens again on Clean
      junk; Run there turns hibernation off (`powercfg /h on` undoes it)
      and the output shows in the row. On Linux Run on the apt cache asks
      for the password. Clean shows the progress page with each rule;
      Stop stops after the current one; the results page says what was
      freed and skipped, and Open the quarantine shows one "Clean junk"
      batch.
- [ ] Quarantine explorer (0.17.0): a batch opens to its items grouped by
      rule or folder; a folder in it opens and the path at the top goes
      back; Open shows a picture, an `.exe` only has Show in folder; Put
      back on one item restores just that one; with a file of the same
      name back in place, Put back asks, and Keep both makes
      "name (put back).ext", Replace puts the other one in the quarantine;
      Delete for good on one item frees its space and keeps the rest.
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
- [ ] Free up space (0.17.0): one Clean junk card with the total; Review
      opens the Clean junk screen with the trash, old installers,
      thumbnails, leftovers, old downloads, screen recordings and empty
      folders as there are any; only thumbnails and empty folders start
      ticked; Clean shows the progress and results pages; what's moved
      lands in the quarantine as one batch that opens like on a desktop,
      and the tabs and buttons are easy to hit.
- [ ] The trash (0.20.4): with a photo deleted in the gallery, ticking
      Trash and cleaning shows Android's own dialog; Allow frees it and
      the results page says freed, not quarantined; Don't allow leaves the
      row marked as not confirmed, nothing else is touched either way.
- [ ] Pairs from the QR code; a delete from the Hub is approved on the
      phone.
- [ ] The QR code fills in the connect dialog both ways (0.15.1): with
      FreeSize closed (swiped away), and with FreeSize open in the
      background; scanning it twice doesn't open the dialog twice.
- [ ] In the background (0.14): with the app closed for a day, the Hub
      shows the phone seen within 15 minutes all day and its disks and
      quarantine up to date; a scan made while the Hub was unreachable
      arrives once it is; a request from the Hub, and an approval for a
      server the phone approves for, show as a notification within 15
      minutes, and tapping it opens the app on the request; the battery
      screen doesn't list FreeSize above the usual apps.
- [ ] Updates: a newer release is offered and opens for installing.

## Hub

- [ ] A device's Changes tab: the chart has a point per kept scan, a click
      compares that scan with the newest, and rows have bars.
- [ ] Starts fresh: makes its key and certificate, the dashboard signs in
      through Authentik over HTTPS.
- [ ] Upgrades from the previous version: devices, scans, alerts and the
      audit log are all there; `/api/audit/verify` passes.
- [ ] A device behind the Hub's version is flagged; the Hub says when a
      newer release exists (with the update check on).
- [ ] Alerts reach a channel; the message carries the audit log's head.
- [ ] Alerts (0.21): Acknowledge greys an alert, the device card stops
      counting it and nothing more is sent; Snooze for 1 day brings it
      back (and sends it) a day later; Show again undoes either; when it's
      over the channel still says so; Clear empties "Over"; each is in the
      audit log with your name. Viewers see no buttons.
- [ ] Cards (0.21): "Scanned … · Checked in …" match what each device did
      last; ● when online, ○ when not; a device not scanned for 7 days
      shows ⚠ out of date, and 0 in When to alert turns the mark off. A
      new scan or junk check on a device appears on its Activity tab, and
      the same report again doesn't add a line.
- [ ] A device's Junk tab shows what its cleaner found; Clean on a safe
      rule is approved on the device and the tab updates; a junk alert
      fires above the limit set on the Alerts page.
- [ ] Junk tab: Tick the safe ones, Ask to clean… lists them; the device
      shows one request per rule, and each cleans its rule once approved. Largest files: pick a type, tick the header box, and Ask to
      delete… lists only that type's files.
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

## Nextcloud app (0.20)

- [ ] Unpacked into `custom_apps` and enabled, Administration settings →
      FreeSize pairs with the Hub's link (over HTTPS with the Hub's own
      authority), and "Nextcloud" shows on the dashboard.
- [ ] Approver: the phone's fingerprint is accepted, the device page names
      the phone; a request without an approver is refused.
- [ ] With two users added (each with an email), the Hub's Nextcloud tab
      lists both for an admin; signed in to the Hub as one of them, only
      their own account and requests show.
- [ ] A file copied to a second folder shows under Extra copies once the
      app has fingerprinted it; `cmp` on the server says the two are the
      same.
- [ ] Move to the trash bin… on the extra copy: the phone asks, and once
      allowed the file is in that user's Deleted files, not deleted for
      good; Restore brings it back where it was.
- [ ] With the Deleted files app off, the page warns and the request is
      refused.
- [ ] (0.20.7) With FreeSize open on the phone, a Move to the trash bin…
      request opens the approval dialog within seconds, before the
      Nextcloud has looked; with FreeSize switched away from (not closed),
      a notification comes instead and opens the dialog. Once allowed, the
      file is in Deleted files after Nextcloud's next cron run (up to
      about 5 minutes). Withdrawing a request on the dashboard closes the
      dialog on the phone.

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

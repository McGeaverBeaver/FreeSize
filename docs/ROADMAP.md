# Roadmap

What's planned, in rough order. Nothing here is built yet unless listed under Done.

## Ground rules for every part

These hold for the phone app, the dashboard, the Unraid plugin and anything
else that can delete:

- **Nothing is ever deleted automatically.** Scans, schedules and rules only
  suggest. A person always chooses what goes.
- **Look before deleting.** Every suggestion lists the exact paths, and each
  can be opened to browse the files inside before anything happens.
- **Recoverable first.** Where possible, deleting moves items to a quarantine
  folder on the same disk (so it's instant and frees space only when
  emptied), with one-click restore. It's emptied only when someone asks,
  never on a timer.
- **Protected paths are never offered.** Each platform has a built-in list
  (for example Unraid's flash drive, `system` share and the appdata of
  installed containers), and admins can add their own.
- **Everything is logged:** who asked, who approved, what ran, the result.
- **A grant never covers what can't be undone.** Emptying the quarantine
  and removing Docker images are asked for every time, with the paths
  shown, however long a grant for deleting is still running.

## Done

- 0.1: scanner, tree, treemap, desktop and Android apps.
- 0.2: app details, unused / never-opened / not-from-a-store groups,
  uninstall, Save APK, app history, clear-cache dialog.
- 0.3: Free up space (trash, old installers, thumbnails, leftovers), files
  saved by apps with Browse, biggest caches, daily size recording, the
  usage-history fix.
- 0.4 (phases 2b and 2c): storage chart with a forecast, Growing fast apps,
  drive trends on every platform; the Clean up tab with rebuildable folders,
  files not changed in a year and a duplicate finder; `--cleanup` and
  `--duplicates` on the command line.

- 0.4.1: Open the selected file in its usual app (on Android through
  MediaStore, so viewers can read it).
- 0.5.1: tap a file type to list its files.
- 0.5 (phase 2d): icons on the treemap (app, game or kind of content),
  the bubble view with zoom and a tappable path, green outlines on
  rebuildable folders, app and game icons for folders on every platform
  (Android packages, Steam, Linux desktop entries, Windows programs, macOS
  bundles), and app groups as bubbles on Android with each app's parts.
- 0.6 (phase 2e, found by comparing with SpaceObServer, TreeSize
  Professional, WizTree, WinDirStat, QDirStat, DaisyDisk, SD Maid SE, Files
  by Google and others):
  - folder sizes kept per scan and the Changes tab comparing any two scans,
    with growth shown on the bubbles;
  - files by year changed and by size band, each tappable;
  - system and app caches on Windows, macOS and Linux (`Windows.old`,
    update caches, browser and shader caches, …), and space no file
    accounts for (ZFS snapshots, Windows shadow copies, Time Machine
    snapshots);
  - the basket (now the selection): collect from any view, review once, delete all or move all
    (keeping folders, checked copies across drives, never overwriting) with
    Put back;
  - export as an HTML report, CSV or an ncdu-compatible scan file, opening
    saved scans and `ncdu -o` files read-only, and `disktree --json`,
    `--export`, `--import`;
  - a similar photos finder (difference hashes, read from EXIF previews);
  - Android cards to review old screenshots and chat apps' media.
- 0.7 (phase 3): the DiskTree Hub.
  - A server with a web dashboard, sign-in through Authentik (OpenID
    Connect with PKCE), admins, viewers and viewers limited to device
    groups.
  - Pairing by QR code or link, checked by the Hub's key fingerprint.
    Devices sign every request, the Hub signs every command.
  - The command-line agent for servers, and the Hub card in the desktop
    and Android apps.
  - Requests to delete, clean up or uninstall, approved on the device
    (Deny / once / for N minutes) or, for servers, on their approver, which
    only the server can set.
  - A hash-chained audit log.
  - History with retention, comparing any two scans, search across devices
    with saved searches, CSV/JSON/HTML exports, and devices without an
    agent fed with ncdu files.
  - Alerts (full, growing fast, forecast to fill, quiet servers, waiting
    requests) to ntfy, Gotify, Pushover and webhooks.
  - Prometheus metrics and JSON for Home Assistant.
  - A Docker image for amd64 and arm64, the Unraid template, and systemd
    units.
- 0.8 (phase 4): DiskTree for Unraid, a plugin with a page under Tools.
  - Disks and pools scanned directly (`/mnt/diskN`, never `/mnt/user`),
    without waking sleeping disks; each share's size per disk.
  - Docker (`docker.img` or a folder) and VMs: safe to remove, check first,
    or in use and never offered; unused images and the build cache removed
    on request.
  - Never offered: the flash drive, the system share, Docker's storage,
    installed apps' appdata, container mounts, VM disks and ISOs in use,
    whole disks and shares, remote mounts, open files, and the owner's own
    list.
  - A quarantine on each disk, with put back and empty only on request;
    the command-line agent on other servers uses it too.
  - What deleting really frees (hard links, ZFS snapshots), what keeps
    disks spun up, a permissions report, and moving a share between disks
    with a plan and checked copies.
  - Reports to the Hub (its Unraid and Quarantine tabs); the server's
    approver is chosen on the page or with `disktree approver set`.
- 0.8.1: the App caches card says how much Android's cache cleaner can
  really clear, and offers it only when that's worth it.
- 0.8.2: each group of apps opens as its own page with a way back, instead
  of changing a list further down the screen; rows show when they're pressed.
- 0.8.3: the Hub's Docker image (amd64 and arm64) is published with each
  release, and releases include the Hub and command line for ARM boards.
  The Unraid plugin's page fits the screen (Unraid's table styles kept
  every cell on one line and pushed the buttons off the page), groups
  Docker's findings into safe, check first and in use (folded), and until
  the first look is done says what it's looking at instead of that Docker
  and the VMs are off. Moving between disks has a folder picker that shows
  which disks hold each folder and ticks them; a folder on several disks
  moves from each, one after another.
- 0.8.4: the Hub signs people in through Authentik providers without a
  signing key (HS256 with the client secret). Emptying the quarantine and
  Docker cleanup are action kinds of their own that no grant covers. The
  Unraid plugin works out what's protected when a request arrives, not at
  the last round. A scanned folder that is a link can't lead a delete out.
  The Hub refuses paths that aren't in the device's newest scan. Pairing
  over plain `http://` to another machine warns. A damaged device key is
  refused rather than replaced. The README reads the client secret from a
  file and says what a device sends.
- 0.8.5: every button on the Unraid plugin's page works again. Unraid checks
  the page's token itself and removes it before the plugin's PHP runs, so
  the plugin's own second check refused everything with "the page is out of
  date"; a test now runs the page's PHP the way Unraid does. Pairing on the
  page is logged and its error stays on the page, reason first. On Unraid,
  `disktree agent` and `disktree approver` use the plugin's pairing without
  `--state`. The Android app can reach a Hub at a plain `http://` address
  and shows Java's reason when it can't reach one. The dashboard's Copy link
  works over plain http.
- 0.8.6: DiskTree is called FreeSize. Crates, binaries (`freesize`,
  `freesize-gui`, `freesize-hub`), the Docker image
  (`ghcr.io/mcgeaverbeaver/freesize-hub`, also pushed under the old name),
  the plugin (`freesize.plg`), the systemd units and the Android package id
  (`io.github.mcgeaverbeaver.freesize`, same signing key). What keeps
  working from before:
  - data folders on Windows, macOS and Linux (the app, the agent, a Hub
    outside Docker) are renamed into place on first start, and the phone's
    app history in `Documents/DiskTree` too;
  - `DISKTREE_…` settings count where the `FREESIZE_…` name isn't set, and
    the `disktree-admins` and `disktree-viewers` groups while the group
    settings are left at their defaults;
  - `disktree://` pairing links (the phone opens them too);
  - on Unraid, the old plugin's update (`disktree.plg`) installs
    `freesize.plg`, which copies `/boot/config/plugins/disktree` (with
    `disktree.cfg` as `freesize.cfg`) and removes the old package, page
    and `.plg`;
  - names that stay: the protocol's headers and signing labels, the
    quarantine folder `.disktree-quarantine` (its manifests hold full
    paths), the Prometheus metrics, the webhook's `X-DiskTree-Signature`
    and the dashboard's cookie.
  The phone pairs again: a new package id is a new app, and its private
  storage (the pairing key) can't carry over. Still to check on real
  devices: a 0.8.5 plugin, Hub, Windows app and phone each upgrading with
  pairing, settings, history and quarantine intact (the plugin's update
  through `disktree.plg` above all, since Unraid's update can't be run
  here).
- 0.8.7: HTTPS by default. The Hub makes its own certificate authority on
  first start (in the data folder next to its key) and a certificate for
  the host in its address: SHA-256, the host in the subject alternative
  names, server authentication, 825 days (Apple's limit), renewed a month
  before it ends or when the address changes. Devices learn the authority
  when they pair (the Hub's signed pairing answer names it, and the link
  vouches for the Hub's key) and trust it for this Hub's address only:
  ureq with exactly that root on desktops and servers, a trust manager
  loaded with it through JNI on Android. `FREESIZE_TLS_CERT_FILE` and
  `FREESIZE_TLS_KEY_FILE` take an administrator's certificate instead,
  reloaded when the files change; `FREESIZE_TLS=off` keeps plain http for
  a reverse proxy. Plain http is still answered on the same port: the
  browser is sent to `https://`, devices paired before are told the new
  address in a signed answer and move over once the Hub answers there with
  the key they paired with, and the health check keeps working. The
  dashboard's Settings shows the certificate and offers the authority to
  install; pairing links and the Authentik redirect URI are `https://`.
  Tests: the certificate meets Apple's rules; a device refuses another
  certificate for its Hub's address and the system's roots refuse the
  Hub's; the authority isn't trusted for another name; a move must carry
  the Hub's signature and the Hub's key at the new address. Still to check
  on real devices: the phone and the Unraid plugin pairing from the QR
  code and the link, a 0.8.6 device moving over, an administrator's
  certificate and a proxy setup. The Authentik redirect URI has to be
  changed to `https://` by hand on a Hub that was `http://`.
- 0.8.8: the whole disk on every platform. A scan names the folders it
  couldn't read (`Tree::unreadable`) at the top of the Clean up tab, in the
  command line's output and in the report to the Hub, which stores them and
  marks the scan incomplete, with the count, on the device, the scans table
  and the folders page. Devices report whose eyes they scan with (user,
  administrator, root); the dashboard says a Windows agent sees what its
  user sees. The card offers the fuller scan: Windows relaunches the app
  through UAC (`--administrator`), turns on the backup privilege and says
  "administrator" in the title, naming what stays out of reach (System
  Volume Information, the reserved storage); macOS checks Full Disk Access
  by opening TCC's database and opens the setting, then scans as root
  through an administrator prompt; Linux through `pkexec`; both run
  `freesize-gui --export` without a window and open the result as a saved
  scan, read-only. Android: nothing new. Tests: a folder without read
  permission is counted, marked and named in the card and in the Hub's
  scan; the launch modes parse; root is offered nothing more. Still to
  check on real machines: the administrator scan's totals against the
  drive's used space on Windows (and whether `FindFirstFile` honours the
  backup privilege), the password prompts on macOS and Linux, and Full
  Disk Access following an unsigned build.

Left over and planned (the release it goes in is under Next releases):
- **Shrinking photos** (SD Maid SE's Squeezer) now that the quarantine
  (0.9) can hold the originals, since it rewrites people's photos.
- **Shizuku mode** on Android: batch cache clearing and `Android/data`
  through an unrooted phone's helper. The Java side the APK gained in 0.14
  (the background job) is the foothold; Shizuku's client library (an AAR
  with its AIDL, fetched by `build-apk.sh`), its provider in the manifest,
  the permission flow and a user service running as shell are still to
  write, and need a phone with Shizuku to try. Sideloaded builds only; Play
  restricts it.
- From 2b/2c: **leftovers of uninstalled programs** in AppData and
  `~/Library`: done in 0.15, with the record of which folder belonged to
  which program as the reliability it waited for. What a program's
  folder is called is still only known once FreeSize has seen the program
  installed, so the rule finds more the longer FreeSize is around; a
  look at a fresh machine finds only the id-named folders.

Parked, each with its reason:
- **The Hub on Windows** as a service from the installer: needs the
  installer (NSIS) and a Windows service wrapper, tested on real Windows.
  The Hub runs in Docker Desktop meanwhile.
- **An Apps bubble view on desktops** and **Epic and GOG game artwork**:
  nice, not asked for.
- **Removing packages** (apt, dnf, Flatpak, Snap, winget, Homebrew) and
  **Docker cleanup outside Unraid**: each needs its own checks of what's
  safe; deleting files, cleaning junk and uninstalling Android apps come
  first.
- **Scanning over SSH** without an agent: uploading `ncdu -o` files covers
  it, without the Hub holding SSH keys.
- **Mass-change warnings** (thousands of files rewritten, a new extension
  everywhere): needs file-level changes between scans, which the summaries
  don't keep.
- **Email alerts and scheduled digest reports**: ntfy, Gotify, Pushover and
  webhooks (which can reach email through Home Assistant or n8n) cover it.
- **Disk health from Scrutiny**, **S3/B2/MinIO buckets**, **per-user
  totals** for shares and Nextcloud, **tags and keep marks** on what's
  found, and **MQTT discovery** for Home Assistant (the REST sensor works):
  each is for someone who hasn't asked yet.
- **An MCP server** for AI assistants: read-only first, with actions still
  approved on the device; after the things above.
- **Blurry photos**: a sharpness score per photo, on top of the similar
  photos finder, to pick the best of a group.
- **NTFS compression in place** and **fast Windows scans** from the Master
  File Table and change journal (as WizTree does): both need administrator
  rights and a real Windows machine with NTFS drives, and the fast path only
  pays on NTFS.
- **Container logs, writable layers and orphan volumes** stay only shown:
  Docker keeps writing to a running container's log, a layer is part of the
  container, and a volume's data can't go to the quarantine without Docker
  losing track of it.
- **Unraid's own notifications** for full disks: the Hub's alerts cover it
  once paired.
- **Editing the Nextcloud client's configuration** to add a sync folder in
  place: one wrong write makes the client resync or forget a folder, and the
  client has no supported way. Dropped from phase 5.

- 0.9: trust and updates (phase 7). The license is GPL-3.0-or-later
  (`LICENSE`, every crate); `SECURITY.md` says what leaves a device, the
  threat model and how to report; `TESTING.md` is the by-hand checklist
  per release; issue templates for bugs and ideas; a CI workflow runs fmt,
  clippy and the tests on Linux, Windows and macOS on every push (the APK
  and the Unraid plugin only when started by hand). Signed releases:
  `release.sh` signs `SHA256SUMS.txt` with the release key (`freesize
  release key|sign|verify`), the apps carry the public half from
  `release/public-key.txt`, and `freesize_proto::release` checks it. The
  update check (`freesize_agent::update`): opt-in on each device (the home
  screen's Updates section, with a beta channel), or the Hub checks once a
  day with `FREESIZE_UPDATE_CHECK=on` and tells older devices in their poll
  answers; Update downloads the sums and signature, refuses anything the
  release key didn't sign, downloads the platform's file and checks it
  against the sums. Windows and Linux swap `freesize-gui` and `freesize`
  beside the running program and restart; Android saves the verified APK
  in `Download/FreeSize` and opens it in the package installer, which asks
  (and refuses another signing key); `freesize update --install` does the
  same for the command line. The quarantine on desktops and phones: every
  delete in the app goes to it (in the data folder for the disk it's on,
  `.disktree-quarantine` at the top of other disks and of the phone's
  storage), the Clean up tab lists batches with Put back and Empty, Hub
  requests land there as a batch under the request, and the dashboard's
  Put back and Empty work on every device now. The Hub marks devices
  running an older version, writes the audit log's head to its log once a
  day and into every alert, prunes scans and disk samples past
  `FREESIZE_RETENTION_DAYS`, and limits wrong pairing codes (10 a minute
  per address, 20 in all) and sign-ins (30 and 300). Still to do by hand:
  the owner makes the release key on the build machine and commits
  `release/public-key.txt` (until then releases are unsigned and apps
  won't install them themselves); registers the package and the signing
  key with Google under a limited distribution account; and tries a 0.9.0
  phone, Windows PC and Linux box updating to 0.9.1, an edited sums file
  being refused, and Put back on the phone restoring byte for byte.
- 0.9.1: out in the open. The Community Applications files: `unraid/ca/
  ca_profile.xml` (who the templates come from), `unraid/ca/freesize.xml`
  (the plugin's entry, pointing at the release's `freesize.plg`, support
  on GitHub issues) and the Hub's template with its categories and
  license; CA scans the repository for them once the owner submits it
  through Squid's form (forums.unraid.net, "CA Application Policies"),
  which also wants 2FA on the GitHub account. The README is the user page
  now (installing, the command line, using the app, the phone, updates,
  the Hub, Unraid, licensing) and `docs/DEVELOPING.md` the developer page
  (building, trying the Hub, releases, the keys, the project layout).
  Still to do by hand: submit the repository, open a support thread on the
  forum if CA asks for one (then put its address in the entry and the
  `.plg`), and check both appear in the Apps tab on a clean Unraid 7.3 and
  install from there.
- 0.14: the phone in the background. The APK holds one Java class,
  `HubJob`, a `JobService` Android runs about every 15 minutes once the app
  is paired (persisted across reboots, needing a network, scheduled through
  JNI). It calls into Rust (`crates/app/src/background.rs`), which sends the
  reports the app couldn't (kept in `hub-pending/`), a fresh report (disks,
  quarantine, the Nextcloud folder; apps and junk every six hours) and
  polls once; requests for the phone are marked shown and come back as
  notifications that open the app, where they're approved with the paths
  shown as always; approvals for other devices too. Nothing runs from the
  background, not even under a grant. `build-apk.sh` compiles the Java and
  repacks cargo-apk's APK with it and the hand-written manifest
  (`crates/app/android/AndroidManifest.xml`). Not done: Shizuku mode
  (batch cache clearing and `Android/data` through Shizuku's helper) needs
  Shizuku's client library in the APK and a phone with Shizuku to try it
  on; it's listed under Left over. Still to do by hand: the day-long check
  (reports within 15 minutes with the app closed, an approval notice within
  15 minutes, the battery screen) in TESTING.md.
- 0.15: leftovers of removed programs, the 2b/2c item left over. A
  "check first" rule of the junk cleaner (`program-leftovers`, under
  Apps, `freesize_core::leftovers`) for the settings and caches in
  AppData, `~/Library`, `~/.config`, `~/.cache`, `~/.local/share` and
  `~/.local/state` of programs that are gone. The reliability the item
  waited for comes from two sources: the installed programs are read the
  platform's way (the Uninstall registry keys, Spotlight's list of app
  bundles, `.desktop` files and the dpkg, pacman and rpm databases, Flatpak
  and snapd) and every look records which folder belonged to which program
  while it was installed (`programs.json` in FreeSize's data folder,
  shared by the app and `freesize agent`), so a folder is only called a
  leftover once its program was seen installed and is gone, or when it's
  named by the program's own id (Flatpak, snap, bundle ids) and that list
  was read completely. Nothing changed in two weeks, a believable list of
  programs, and a list of names programs share (`Microsoft`, `fonts`,
  Apple's ids) that is never offered. Each path says which program and
  when it was last seen; cleaning moves the folders to the quarantine. In
  the Clean junk screen, `freesize junk` (`--also program-leftovers`,
  with the programs in `--json`) and the Hub's Junk tab. Still to do by
  hand: the check in TESTING.md on a Windows machine and a Linux box.
- 0.15.1: scanning the Hub's QR code with FreeSize already open brought
  the app to the front without the link (found in testing, 2026-10-13):
  the app's `NativeActivity` runs in `singleTask` mode and drops links
  delivered to a running activity. The link filter is now on `PairLink`, a
  Java activity that shows nothing, leaves the link in the app's files
  folder and opens the app, which picks it up at start and while running.
  Also: the desktop Clean junk screen wouldn't let "check first" rules
  (installers and old downloads, screen recordings, the leftovers) be
  ticked, although cleaning them was always meant to move them to the
  quarantine; they start unticked and can now be ticked
  (`Finding::can_tick`). Rules removed with the system's own tool, and
  rules whose program is running, still can't. Still to do by hand: the
  QR check in TESTING.md, and ticking a "check first" rule on Windows.
- 0.16: the Clean up tab redesigned for everyday use (asked for on
  2026-10-13), on desktops and phones alike. An overview of cards (how
  much each could free, how many are ticked) opening to a page per card, tick boxes on every row and
  buttons to tick many at once: every extra copy of the duplicates,
  keeping one outside caches, not named like a copy and oldest (marked
  Keep), duplicates of one type only (EXE, JPG, …), all but the largest of
  each set of similar photos, shown side by side as tiles, the safe system
  caches. Ticking fills the basket, now called the selection, whose review
  lists everything once more; a set with every copy ticked is flagged on
  the page and in the confirmation. The Hub's Junk tab ticks several rules
  and asks for them in one dialog, and its Largest files tab lists one type
  and ticks all it lists. Still to do by hand: the Clean up checks in
  TESTING.md on Windows and on a phone.
- 0.16.1: on a phone every delete failed, and so did every Free up space
  card (found in testing, 2026-10-13): all of them move to the quarantine,
  which was put at the top of the filesystem shared storage is on,
  `/storage/emulated`, where apps can't write. It now goes at the top of
  the storage root the item is in (`/storage/emulated/0`, or an SD card's
  own top), as was always meant.
- 0.16.2: CI had never passed since it was added in 0.9: tests assumed
  Linux paths (Windows separators, macOS's temporary folder under
  `/private`, which deletes refuse). Fixed, and it runs on master pushes
  only, no longer again for each release tag. Found by it: on a desktop
  where FreeSize hadn't saved anything yet, the first delete went to the
  top of the disk and failed; the data folder is now made first. And on
  Windows and macOS every https request crashed (the Hub since 0.8.7,
  the update check): ureq was built without its `native-tls` feature.
  Releases stop making `disktree.plg` and tagging the image as
  `disktree-hub`: every install runs FreeSize now. The last of each stays
  where it is.
- 0.17.2, polish: layout. Next to the desktop's list of places the folder
  tree had room for one letter of each name; it drops the percent column
  when narrow. Treemap names are cut short with "…" instead of breaking
  mid-word, the Treemap/Bubbles switch sits above the map instead of over
  it, and a bubble's label sits low on its rim, off the bubbles inside.
  "Not changed in over a year" has its size, duplicate rows each show
  theirs, and on a phone a card's size goes under its title instead of
  disappearing. Selecting no longer also writes the same message in the
  status line (the page and the selection bar say it). Quarantine rows are
  titled by what's in them, with when and who below, sizes on a phone and
  a gap between rows. The Hub at phone width: the menus wrap onto a second
  line, Docker and VM finds put their badge under the name and the button
  under the size, the Junk tab's per-rule Clean… gives way to tick and Ask
  to clean, wide tables scroll inside their card, the pairing code sits
  under the QR code with a Copy server command button, the request alert
  is a field in its sentence like the others, the quarantine's Unraid
  sentence shows only for Unraid and a device without buttons says why,
  and the Unraid tab no longer fails on a server that hasn't reported yet.
- 0.17.1, polish: words, against CONTEXT.md. Buttons that add to the
  selection say Select … and the ones that take back out Unselect …
  (a duplicate set's own button is Select extra copies, a photo set's
  Select all but the largest, both turning into Unselect; the page's Clear
  is Unselect all); the selection bar's Review… is Review selection…, so it
  isn't read as a card's Review. The phone's scan tabs are Largest, Types
  and Clean up (Clean was also the name of a place). Put back "returns"
  instead of "restores"; the quarantine no longer says "set aside by
  deleting" or "deleted here". The Hub sheet in the app shows the
  connection as a plain line rather than a yellow warning, spells out the
  approver command without backticks and says "Every request from the Hub
  asks you first" instead of "No grants". The Hub's Unraid tab no longer
  shows "parity parity" and puts a share's cache setting in words. Delete
  keeps its name (the owner's call) and the Hub keeps "batches" (a
  glossary word).
- 0.17.0, polish: Clean junk and the quarantine, on desktops and phones
  together (the owner's review, 2026-10-13: "Show command" showed nothing,
  rules that can't be cleaned had tick boxes, and Clean went to a page
  that looked like a scan). Rules left to the system's tools moved to
  their own section at the bottom with no tick box and out of the total;
  Show command opens the command in the row with Copy and Run, Run asks
  first (the command, what it changes, how to undo it) and shows the
  output, and "Run as administrator" reopens FreeSize as administrator on
  Clean junk on Windows (`--junk`) or asks for the password on macOS and
  Linux. A summary card says what Clean frees; Clean shows a progress page
  (a bar, each rule's state, Stop after the current rule) and then a
  results page. One Clean makes one quarantine batch, "Clean junk", with
  its items grouped by rule. A quarantine batch opens like a file
  explorer, with Open, Show in folder, Put back and Delete for good on
  each item, tick boxes for several, and folders to browse; programs and
  scripts only get Show in folder. Putting back over a name that's taken
  asks Keep both, Replace or Skip. The phone's per-kind junk cards became
  one Clean junk card opening the same screen.
- 0.16.4, polish: mistakes around deleting. Clean junk ticked the trash
  by default and emptied it for good while the confirmation spoke only of
  caches; the trash now starts unticked and the confirmation names it.
  The selection bar kept the old count after a new scan (the selection
  itself was empty), so ticking seemed to replace it. Deleting several
  items made one quarantine batch each; it's one batch now, put back
  together. Similar photos and duplicates showed file lengths while the
  selection counted size on disk (883 KB against 888 KB); both use the
  scan's sizes now. The Hub's junk alert said "0 B of it is safe to
  clean".
- 0.16.3, the first of the polish (the owner's call, 2026-10-13: everyday
  people on a phone or Windows PC first, no new features until it's
  polished). The one long scrolling home screen became places: a list on
  the left on desktops and tabs along the bottom on phones (Home, Clean,
  Apps on phones, Quarantine and Hub on desktops, Settings). Settings
  holds the junk reminder and update checks as drop-downs, where the
  reminder used to take a whole card and its button named the next
  setting rather than the current one. The quarantine lists batches newest
  first with their names. On Windows, **Run as administrator** sits beside
  Scan whenever FreeSize isn't, and the window that asked closes once the
  new one opens (also from Clean up's fuller scan), unless it's still
  moving or deleting something. The Hub's Copies page listed nothing on
  the Nextcloud side for an account signed in by email, so every synced
  folder looked like it had only one copy: the WebDAV path takes the
  account's id, which the Hub now asks Nextcloud for and keeps
  (`nextcloud.dav_user`).
- 0.13: copies, actions (phase 5, second half). The Copies page's
  Suggestions tab turns the join into requests, each still shown and
  approved on the device: free a folder's local copies on Windows (the
  cloud files API's pin attributes, which the Nextcloud client honours on
  its next run and only for what it has uploaded; macOS and Linux are
  pointed to the client's own menu), with Free and Keep here per device
  on the Folders tab; camera files a phone still holds after the app
  uploaded them, matched by name and size, to the phone's quarantine; a
  big folder outside any synced folder moved into the sync folder as
  `Archive/<device>/<folder>` (a rename on the same drive, else a copy
  checked byte for byte with the original quarantined; the Hub makes the
  folder on Nextcloud first with MKCOL and reads the account's free
  quota); and on Unraid a share the Nextcloud container mounts added as
  external storage through `occ files_external:create` in the container,
  which the plugin reports along with the container and its mounts. Still
  to do by hand: the 10 GB folder on a Windows PC becoming online-only
  after the next sync and coming back on demand (TESTING.md).
- 0.12: copies, read only (phase 5, first half). Devices with the
  Nextcloud client report their sync folders (`freesize_agent::nextcloud`:
  the desktop client's `nextcloud.cfg`, the Android app's folder; per
  folder the size, what's stored on the device, subfolders two levels down
  and files of 100 MB and over), hourly from the app and the agent, and
  `freesize nextcloud` prints it. The Hub connects a Nextcloud account
  through Login Flow v2 and lists it over WebDAV (folder sizes, big files'
  ids and checksums, the trash) at start, every six hours and on request;
  the Copies page joins the two on server, account and path: "kept on N
  devices" per folder with each device's stored bytes, the big files with
  the server's id, and the "only one copy" report (a file or folder one
  device alone holds that the server lacks, folders only the server
  holds, big scanned folders no synced folder covers). The Hub reads only.
  The client's sync journal isn't read (an SQLite file the client keeps
  open): paths are the key, and the Hub resolves them to file ids. Still
  to do by hand: the counts for 20 files on three devices, and a file kept
  on one device in the risk report (TESTING.md).
- 0.11: where the space goes, and balancing (phase 8 and Unraid). The
  plugin finds Nextcloud's data folder (`.ncdata`, in a share or in
  appdata) and reports, per user, files against old versions, trash,
  uploads and cache, plus previews and app data, with the retention
  settings from `config.php` and how much is older than they allow;
  nothing in it is offered for deletion. The mover keeps sparse files
  sparse, copies extended attributes, cleans partial copies an interrupted
  move left, shows the file being copied, and takes a tick to delete the
  originals once every copy checked out. Presets: Empty a disk (each
  share to the roomiest disk its settings allow, folder by folder when it
  must) and Gather a share, as plans of moves run one after another, on
  the page and as `freesize unraid move preset`. A round indexes each
  disk's scan (folders, plus the files the later steps look at) and drops
  the tree, and logs its peak memory. The hidden space explanation adds
  btrfs snapshots (exclusive bytes with quotas on), other APFS snapshots
  and macOS's purgeable space. Still to do by hand: compare the Nextcloud
  numbers with `du` on a real server, move a stopped VM's sparse vdisk and
  check its holes stayed, and read the peak memory in the log after a
  round on a big array (TESTING.md).
- 0.10: the junk cleaner (phase 6). `freesize_core::junk`: rules per
  platform (38 on Windows, 34 on macOS, 28 on Linux, 4 on Android, 2 on
  Unraid) in six categories, each with a blurb, a refills flag, a safety
  (safe, check first, use the tool), the processes that mean its app is
  running, and a fixture test; generic rules come last so nothing is
  counted twice. The Clean junk screen on desktops (Find junk on the start
  screen, categories with tick boxes, paths with Show, the tool's settings
  page or command for what the system should remove itself, Clean after
  the shared confirmation, the result with what was freed, quarantined,
  skipped and will refill, a weekly or daily reminder that only says).
  `freesize junk` with `--clean`, `--also`, `--only`, `-y` and `--json`.
  `freesize agent` reports junk every hour and the Hub asks to clean a
  rule like any request; the Hub stores each device's junk (migration 6),
  shows a Junk tab per device and the total on its card, and alerts above
  `junk_gb` (2 GB by default). The phone's Free up space cards gain old
  downloads, screen recordings and empty folders. Unraid: `.Recycle.bin`
  and `.Trash-*` in the shares on the Quarantine tab and `freesize unraid
  junk list|clean`. Still to do by hand: on a Windows machine, a Linux
  box and a phone, check the cleaner lists nothing the system's own tool
  would refuse, frees what it says within a few percent, and the result
  shows what was skipped (TESTING.md).
## Polish

Before any public release (the owner's call, 2026-10-13): no new features
until FreeSize is polished for everyday people, first on a phone or a
Windows PC, then home-lab users. The order is the phone app, the desktop
app, the Hub, then the Unraid page. Only fixes, wording, layout and flow.
A screen is done when a newcomer can use it without the README, no button
does nothing and no screen is a dead end, the words match CONTEXT.md, it
works in light and dark and fits a phone, and every delete says where
things go and how to get them back. Shizuku mode, the Hub as a Windows
service and the parked ideas wait until then. The release key, Google's
registration and the Community Applications submission come last.

Decided for Clean junk and the quarantine (the owner's answers,
2026-10-13; shipped in 0.17.0):

- Rules only the system's tools can clean have no tick box and aren't
  counted; they're listed at the bottom under "Clean with Windows' (macOS',
  Linux's) own tools". A rule whose app is open keeps a greyed tick box
  with an "X is open" badge.
- A command shows in the row with Copy and Run. Run asks first, naming
  the command, what it changes and how to undo it; its output shows in the
  row and the rule is looked at again. Without administrator rights the
  button says "Run as administrator": Windows reopens FreeSize as
  administrator on Clean junk, macOS and Linux ask for the password.
  Settings-only rules keep "Open settings", plus Run where a safe command
  exists.
- Clean shows a real progress page (a bar, each rule, Stop after the
  current rule; Close hides it while cleaning goes on), then a results
  page (freed, quarantined with a link, skipped and why, Done). The list
  starts with a summary card: a big "Free X", Clean, and the counts.
- One Clean is one quarantine batch, "Clean junk", grouped by rule.
- A batch opens as an explorer page: per item Open, Show in folder, Put
  back and Delete for good, plus tick boxes. Programs and scripts only get
  Show in folder. Putting back over an existing name asks Keep both (the
  default, "name (put back).ext"), Replace or Skip.
- The phone gets the same design.

## Next releases

Each release is small enough to ship in a few weeks, and each says what
"done" means. 0.8.6 (the rename), 0.8.7 (HTTPS), 0.8.8 (the whole disk), 0.9 (trust and updates), 0.9.1 (Community Applications), 0.10 (the junk cleaner) and 0.11 (where the space goes) are under Done. Trust and the quarantine come before any new way to delete;
the junk cleaner before Nextcloud; Nextcloud in two halves so the read-only
half ships even if an action stalls on a device nobody has.

- **0.9, trust and updates.** Under Done. The license (GPL-3.0-or-later) and a
  SECURITY.md with the threat model and what leaves a device. CI on every
  push for clippy and tests on Windows, macOS and Linux. The quarantine on
  desktops and phones. Signed release sums: `release.sh` signs
  `SHA256SUMS.txt` with a release key and the apps carry the public key.
  An update check and in-app updater on Android (through the package
  installer, which asks), Windows and Linux (download, verify, swap the
  executable, relaunch), opt-in on devices or through the Hub, with a beta
  channel for the people who help test. The Hub shows devices running old
  versions. The audit log's head hash goes to the log and to alerts, so the
  chain has an anchor outside the database. A retention setting. Limits per
  address on pairing and sign-in, since the Hub may be reachable from the
  internet. A TESTING.md checklist per release and issue templates. The
  package and signing key registered with Google under a limited
  distribution account. Done when a 0.9.0 phone, Windows PC and Linux box
  each update themselves to 0.9.1 from the app, an edited sums file is
  refused, and a delete on the phone lands in a quarantine that Put back
  restores byte for byte.
- **0.9.1, out in the open.** Under Done. Community Applications submission for the
  plugin and the Hub template (the license, a `ca_profile.xml`, the plugin
  wrapper XML pointing at the raw `.plg`, a support thread on the forum);
  the README split into a user page and a developer page. Done when both
  appear in the Apps tab on a clean Unraid 7.3 install and install from
  there.
- **0.10, the junk cleaner (phase 6).** Under Done. As described there, with three
  constraints: a fixture test per rule, a blurb and a "refills" flag per
  item, running apps skipped; "check first" items go to the quarantine and
  are never ticked. Done when, on a Windows machine, a Linux box and a
  phone, the cleaner lists nothing the system's own tool would refuse,
  frees what it says within a few percent, and the result screen shows
  what was skipped.
- **0.11, where the space goes, and balancing (phase 8 and Unraid).**
  Under Done. Nextcloud's data folder read on the server (previews, versions, trash,
  appdata, per user, with the retention settings that would have cleared
  them); docker.img writers named per container; the mover keeps sparse
  files sparse and copies extended attributes, cleans stale partial files,
  and offers "empty the quarantine once every copy checked out" as one more
  tick; the empty-a-disk and gather-a-share presets as plans of folder
  moves; a round drops each disk's tree once it's used; progress per file.
  Done when the Nextcloud numbers match `du` within 1%, a stopped VM's
  sparse vdisk moves with its holes kept, and a round on a big array logs
  its peak memory under a set number.
- **0.12, copies, read only (phase 5, first half).** Under Done. Connect Nextcloud
  through Login Flow v2; file ids, sizes and checksums over WebDAV; each
  desktop client's sync journal; the Android app's folder; the Hub joins
  on file id and shows "kept on N devices" per folder, with the "only copy"
  risk report. Done when the count is right for 20 files checked by hand
  on three devices and the risk report lists a file kept on one device only.
- **0.13, copies, actions (phase 5, second half).** Under Done. Online-only on
  Windows, uploaded camera photos to the phone's quarantine, moving a
  folder into the sync folder, creating folders on Nextcloud, External
  storage on Unraid. macOS and Linux show and point to the client. Done
  when a 10 GB folder on a Windows PC becomes online-only after the next
  sync with the files still listed, comes back on demand, and nothing is
  freed before the hash matches.
- **0.14, the phone in the background.** Under Done (Shizuku left over). The Java Worker, approvals as
  notifications, then Shizuku. Done when a phone with the app closed
  reports within 15 minutes of a scan and shows an approval within 15
  minutes of a request, over a day, without the battery screen listing
  FreeSize above the usual apps.
- **0.15, leftovers of removed programs.** Under Done. The left-over 2b/2c
  item, as a junk rule on desktops. Done when, after uninstalling a
  program FreeSize had seen, its settings folder is listed under its name
  two weeks later, nothing of an installed program is, and a removed
  Flatpak's `~/.var/app` folder is listed on the first look.

## After polish

Decided with the owner on 2026-10-14. These wait until the polish is done
(see Polish), in this order.

- **0.18, changes as a chart.** On the desktop Changes tab, the phone and
  the Hub's device page: a line of total used space over time, where
  clicking a point compares with that scan; under it, bars for the
  folders that grew or shrank most, and clicking a bar highlights that
  folder in the tree. The desktop and phone apps keep a small record per
  scan (the total and the top folders) for a year, like the Hub's scan
  history. Done when a year of scans draws on all three, and a bar takes
  you to its folder.
- **0.19, review a folder.** Right-click a folder (long-press on a phone)
  → "Review this folder": tabs for Photos (a grid of thumbnails),
  Documents, Videos, Important and Everything else, each with Open, Show
  in folder and Select. Important flags documents and spreadsheets,
  password vaults (`.kdbx`), keys and certificates, crypto wallets, tax
  and accounting files, email archives (`.pst`) and camera photos: "look
  at these before deleting this folder". Keyword search inside files is
  turned on per review and runs on the device that has the files (the
  agent on Unraid); it starts with text, PDF and Office files and a
  keyword list edited in Settings (tax, invoice, passport, bank, contract,
  password to begin with). The Hub only learns the path and which keyword
  matched, never the text. "Compare with my current files" says how much
  of the folder you already have elsewhere ("92% of these photos are
  already in Pictures") and lists the rest, by content, not name. Done
  when an old PC backup of a few thousand files opens reviewed, the
  compare is right for files checked by hand, and keywords are found in
  each supported format.
- **0.20, the FreeSize Nextcloud app, and cleanup in Nextcloud.** A real
  Nextcloud app (PHP, from Nextcloud's app store or installed by hand),
  so suggestions come from the real files: no downloading and no guessing.
  It pairs with the Hub like any device (the Hub's code in Nextcloud's
  admin settings), shows as a device "Nextcloud" and has its own approver,
  so every delete is approved on the approver's phone. Deletes go through
  Nextcloud, into its trash bin, so its database stays right and files can
  be put back there. The admin picks which Nextcloud users it covers; on
  the Hub each person sees only their own account, admins see all. The
  Copies page gets Nextcloud suggestions: not changed in over a year,
  biggest files, the same file twice inside Nextcloud (by real
  fingerprint), the trash bin's and old versions' space, folders only in
  the cloud and on no device, camera uploads already kept elsewhere. Folder
  review (0.19) works on Nextcloud folders through the app, with
  Nextcloud's own thumbnails and keyword search on the server. Without the
  app, the Hub's current Nextcloud sign-in keeps only the suggestions that
  need no guessing (not changed in a year, biggest files, trash and
  versions); duplicates, keywords and reviews need the app. Done when the
  app pairs, a duplicate it reports is byte-identical, a delete approved
  on the phone lands in Nextcloud's trash and comes back from there, and
  one user can't see another's files.

## Phase 3: DiskTree Hub (dashboard for all devices)

One small Rust server with a web UI and SQLite.

Built in 0.7, apart from what's listed under Done → "From 3".

**Running it**
- Docker image for amd64 and arm64: runs on Unraid, any Linux host,
  Synology, or Docker Desktop.
- An Unraid Community Apps template, `unraid/disktree-hub.xml`, ships with
  the image, so the Hub installs from the Apps tab like any other container.
  It fills in the image, an icon, the web UI link, and fields for the web
  port, the appdata folder (`/mnt/user/appdata/disktree-hub`, mounted as
  `/data` for the database and keys), the address to listen on, and the
  Authentik settings (issuer URL, client ID, client secret, and the admin
  and viewer group names; the secret is a masked field).
- Windows installer option "Also install the DiskTree Hub": installs it as a
  Windows service and adds a firewall rule for its port, limited to private
  networks (and optionally to the WireGuard subnet). Built with NSIS on the
  Linux build machine.
- Plain Linux: a systemd service.
- Listens on one address and port (e.g. `192.168.10.200:9090`); binding it
  to the WireGuard address keeps it off the internet. HTTPS by default with
  the Hub's own certificate (0.8.7), the admin's own, or a reverse proxy in
  front.
- Whether the Hub is reachable from the internet is the admin's choice, so
  the Hub is built as if it were: limits per address on pairing and
  sign-in, secure cookies, a proper sign-out at the provider, and a warning
  when a device pairs over plain `http://`.

**Admin sign-in**
- OAuth 2.0 / OpenID Connect against Authentik (authorization code flow
  with PKCE). Roles from Authentik groups, e.g. `disktree-admins` and
  `disktree-viewers`. Authentik's MFA applies.
- Scoped viewers: a group can be limited to some devices, so family members
  see only their own phone and PC.

**Pairing devices**
- "Add device" shows a one-time code (QR and a short key, valid ~10 minutes)
  holding the Hub address and its key fingerprint.
- Each device makes its own key pair; the private key never leaves it.
  Device messages are signed by the device, commands by the Hub, so neither
  a stolen session nor a fake Hub can drive a device.
- Devices connect out to the Hub, so no device needs an open port.

**Approvals**
- Reading (storage, apps, history) needs no approval. Destructive actions
  (uninstall, delete, empty trash) are approved on the device itself:
  Deny / Approve once / Approve for N minutes. The offered durations and the
  maximum are set per device group in the dashboard.
- A grant covers one admin and one kind of action, expires by itself, is
  shown on the device while active, and can be revoked there.
- Devices without anyone at the screen (servers, Unraid): each has a
  designated approver. That admin is set up and signs in (Authentik) on the
  device itself, and its approval prompts go to the approver's phone.
  **The approver can't be changed from the dashboard**, only on the device:
  with a CLI command (`disktree approver set`), or in the device's own
  settings page where it has one (the Unraid plugin's page under Settings).
  A stolen dashboard login therefore can't redirect approvals to itself.
- Append-only audit log; each entry carries the hash of the previous one so
  tampering shows.

**Agents**

What is built today; the parked list has the rest.

| Platform | Reports | Can do (after approval) |
|---|---|---|
| Android | Everything the app shows; in the background every 15 minutes since 0.14 | Delete (permanently, until the phone's quarantine in 0.9); clean junk; uninstall (Android still shows its own confirmation) |
| Windows, macOS, Linux app | Disks and the folders the person scans | Delete to the Recycle Bin or Trash |
| Linux server (`disktree agent`) | Disks and the folders given with `--scan` | Delete to a quarantine on the same file system; put back |
| Unraid plugin | Disks, pools, shares per disk, Docker, VMs, activity, permissions, the quarantine | Delete to the disk's quarantine; put back; empty; remove unused images and the build cache |

**History and reports**
- Every device sends folder totals after each scan (not every file, which
  SpaceObServer shows costs ~50 MB per 100,000 files), plus the biggest
  files and what changed. Any two dates can be compared: which folders on
  the array grew since August, what's new, what's gone.
- Retention decided up front: daily snapshots for a few months, then weekly
  and monthly, in SQLite.
- Search across all devices ("which device has that ISO?"), with saved
  searches, and tags or "keep" marks that stop suggesting things you've
  decided to keep.
- Per-user totals where a device has several users (shares, Nextcloud).
- Scheduled digest reports and exports (CSV, JSON, HTML).

**Alerts and health**
- Alerts: disk over a threshold, fast growth, forecasted full, and a
  mass-change warning (thousands of files rewritten, or a new extension
  appearing everywhere, as ransomware does).
- Delivered through ntfy, Gotify, Pushover, email or webhooks.
- Disk health (SMART, temperature) next to capacity, read from Scrutiny's
  API where it runs rather than rebuilt.
- Metrics for Prometheus/Grafana and Home Assistant sensors.
- An MCP server so an AI assistant can answer "what's filling the array?" and
  propose actions, which still need the device's approval. Read-only unless
  an admin turns actions on.

**More sources**
- Agentless: scan a box over SSH (Proxmox, a VPS, a router), or import an
  `ncdu -o` file, for machines without an agent.
- Object storage (S3, Backblaze B2, MinIO), whose space costs money every
  month.

## Phase 4: Unraid plugin

Built in 0.8, apart from what's listed under Done → "From 4".

Answers "what's taking up the array?" and never breaks the server.

- **Array-aware scanning:** per disk (`/mnt/diskN`), per pool
  (`/mnt/cache`, others) and per user share, so it shows which disks a
  share's files sit on. Sparse vdisks show both provisioned and actually used
  size.
- **Spin-down aware:** scheduled scans skip sleeping disks or use the last
  scan, so analysis doesn't wake the whole array.
- **Docker-aware**, for both `docker.img` and directory mode:
  - images (used, unused, dangling), build cache, volumes (attached or
    orphaned), container log files, writable layers;
  - appdata folders matched to installed containers and templates in
    `/boot/config/plugins/dockerMan/templates-user`;
  - classified as **safe** (dangling images, build cache, rotated logs),
    **check first** (appdata or volumes of containers that are gone, which
    you may want back) or **protected** (anything a configured container,
    running or stopped, uses).
- **VMs:** vdisks in `domains`, ISOs, snapshots.
- **Protected, never offered:** `/boot`, the `system` share, `docker.img`,
  `libvirt.img`, appdata of configured containers, files open by a process.
- **Deleting:** preview and browse first, quarantine with restore, nothing
  automatic. The quarantine lives on the same disk
  (`/mnt/diskN/.disktree-quarantine`), or the "instant" move becomes a copy
  between disks.
- **What deleting really frees:** files hard-linked elsewhere (the *arr apps
  do this) and data held by ZFS snapshots free nothing; the estimate has to
  count links and snapshots.
- **Scanning `/mnt/diskN` directly**, not `/mnt/user`: the shfs layer is
  slow and every directory read can wake a disk.
- **What keeps disks spun up** (as the File Activity plugin shows), and
  **moving a share between disks** with a plan, a dry run and checksums (as
  unBALANCE does), for balancing rather than deleting.
- **Permissions report:** files not owned by `nobody:users` or with odd
  modes, which break SMB access.
- Packaging: a `.plg` with a page in the Unraid web UI, using the Linux
  agent (static build) and reporting to the Hub when one is set up.

## Phase 5: Nextcloud and "do I have a copy?"

Checked against Nextcloud 35 and desktop client 35 on 2026-10-12.

- **Nextcloud beside the plugin is the supported setup**: the plugin reads
  Nextcloud's data folder on the server's disks, hashes files there and
  never downloads anything to check a copy. Nextcloud elsewhere works
  through WebDAV, read-only and slower.
- **Connecting:** the Hub gets an app password with Nextcloud's Login Flow
  v2 (`/index.php/login/v2`, then polling until the person has signed in in
  a browser). Over WebDAV it reads each file's id (`oc:fileid`, the same on
  every client of that server), size and checksums. Trash and versions have
  no size on their roots: the items are summed, or the folders read on the
  server.
- **Desktop agents** read the client's `nextcloud.cfg` for the accounts
  and sync folders and walk each folder (0.12); what is on disk comes from
  the placeholder itself (no blocks allocated, or the `.nextcloud` suffix).
  The sync journal, `.sync_<12 hex>.db` next to each sync folder
  (`metadata.fileid`, `contentChecksum`, the `flags` table's pin state:
  always local, online-only, inherited, which is what the person asked for,
  not what is on disk) isn't read: it is SQLite, kept open by the client,
  and paths resolve to the same file ids on the Hub. 0.13's online-only
  suggestions go by what is on disk, not the pin state, and set the pin
  attributes themselves.
- **Android:** the Nextcloud app's offline copies under
  `Android/media/com.nextcloud.client/nextcloud/<account>/` (readable with
  All files access) and camera photos already auto-uploaded.
- **Matching** local files to copies on Nextcloud and on other devices:
  name, size and date find candidates, a content hash confirms them. Shown as
  "verified copy" or "likely copy".
- Nextcloud's checksum column stays empty unless a client sent one with the
  upload, and `files:scan` never fills it. A WebDAV `PATCH` on the file
  with `X-Recalculate-Hash: sha256` makes the server compute and store it,
  so the Hub asks for that, one file at a time, above a size that matters;
  no hashing service of its own is needed.
- **Immich** as another source of copies: its API checks photos by checksum.
- **Bitrot check** from the same hashes: content changed while the
  modification time didn't.

Suggestions this enables:

- Files kept on a device that are also on Nextcloud and haven't been opened
  in months: make them online-only (Windows, macOS) or remove the local copy.
- Camera photos and videos already uploaded: remove local copies.
- The same big files on several devices.
- **The reverse, as a risk report:** data that exists on only one device,
  with no copy on Nextcloud, another device or a backup, before cleaning
  anything.
- Nextcloud conflict copies, server trash and old versions.
- Later: OneDrive, Google Drive, Syncthing, and backup tools (restic, Borg,
  Duplicacy) as additional sources of copies.

### The same files on several devices

- **Nextcloud's file id is the key.** Each desktop client's sync journal
  records the server's file id for every file it syncs, and the id is the
  same on every device. The Hub joins the devices' reports on it, so "this
  file is stored on 5 devices" is exact for synced files, with no hashing.
  Agents report per synced folder the bytes stored on the device and what's
  set to "always keep on this device"; very big files one by one.
- Files outside Nextcloud are matched as above, by hash, only above a size
  that matters for space, and cached by inode, size and modification time.
- The Hub groups them: "Photos/2019: 38 GB × 5 devices = 190 GB".
- **Where to keep local copies is chosen per folder**, not per file: "keep
  on the desktop and the server". DiskTree turns that into one suggestion
  per device ("make online-only on the laptop, the old laptop and the
  phone: frees 114 GB"). The defaults keep the copy where it was opened
  last and on devices marked as main ones, and drop it first on laptops and
  phones short of space.

### Moving files into Nextcloud

- **Suggestions:** folders that are big, unopened for months, of personal
  files (videos, photos, archives) and not on Nextcloud yet. Each says what
  it frees on the device and what it takes on the server, against the
  server's free space: "D:\Videos\GoPro: 210 GB, not opened in 14 months,
  not on Nextcloud. Move it there and keep it online-only on this PC."
- **The person chooses the layout** on Nextcloud: under
  `Archive/<device>/`, merged into existing folders by kind (Videos,
  Photos), or the folder's own structure. The Hub connects to Nextcloud
  with an app password (Nextcloud's Login Flow v2) and creates the folders.
- **Setting up the sync**, from least to most invasive:
  1. **Into the existing sync folder** (the default): the device moves the
     folder into its Nextcloud folder (a rename on the same drive; across
     drives copied, checked and the original quarantined). The client
     uploads it; once its journal shows every file synced at the right
     size, DiskTree makes it online-only. The files stay where the person
     sees them, and the space is freed.
  2. **On Unraid, no second copy:** the share is added to Nextcloud as
     External storage (`occ files_external:create` with the `local`
     backend, run in the container), so every device reaches it without
     copying it. Changes on disk need a scan or the mount's "check for
     changes" setting.
- **Online-only on each platform:** Windows like Explorer's "Free up space":
  the unpinned attribute on the placeholders (`attrib +U -P`), which the
  Nextcloud client honours on its next sync run, not at once; the UI says
  so; `attrib -U +P` brings a file back the same way. macOS: the client's
  File Provider (macOS 14 and up) gives no way for another app to evict a
  file, so DiskTree shows what's kept and points to Finder. Linux: client 35
  ships a FUSE-based virtual files mode marked beta, so DiskTree shows and
  points to the client. Android: the Nextcloud app's "available offline"
  setting is private to it, so DiskTree shows what's kept and points to the
  setting; uploaded camera photos are freed through the quarantine, as
  Google Photos' "Free up space" does.

### Safety

- No local copy is freed until the copy on Nextcloud is confirmed by hash.
  With Nextcloud on the Unraid server, the plugin hashes the server's copy
  on disk, so nothing is downloaded to check it.
- Every action is approved on the device it happens on. Online-only comes
  back with a click; deleting goes through the quarantine.
- The "only copy" risk report becomes a check: when everything would end
  up online-only and Nextcloud's own data has no backup, the server holds
  the only copy, and DiskTree says so before suggesting it.

### In this order

1. Read only (0.12): connecting Nextcloud, the clients' sync state on each
   device, "kept on N devices", and the only-copy risk report.
2. Actions (0.13): online-only on Windows, camera photos already uploaded,
   moving into the sync folder, creating folders on Nextcloud, External
   storage on Unraid. Each approved on the device it happens on, and nothing
   freed before the hash matches.

## Phase 6: Junk cleaner on every device

What CCleaner does for Windows, on every device DiskTree runs on and without
breaking anything: one button finds the junk, every item says what cleaning
it means, and the person ticks what goes.

- **One "Clean junk" screen** in the Android app, the desktop apps (Windows,
  macOS, Linux) and the Unraid plugin, and `disktree junk` on the command
  line. It checks the known places directly, so it takes seconds and needs
  no scan of the whole drive first.
- **Categories with a tick box each:** system (temporary files, crash dumps,
  update leftovers, logs), browsers (caches only), apps, games (shader
  caches, download leftovers), developer tools (package and build caches),
  and trash and downloads (the trash, old installers, old downloads and
  screen recordings). Each shows its size and opens to its items and their
  exact paths, which open to browse the files.
- **Ticked only when safe:** items marked safe are ticked, "check first"
  items aren't, and what Windows, macOS or a package manager should remove
  itself (Windows Update cleanup, `apt clean`, `journalctl --vacuum-size`)
  gets a button that opens that tool or shows the command, never a delete.
- **A blurb on every item about the impact**, as the Android cards have now:
  what it is, what happens after cleaning and whether it comes back. For
  example: "Chrome's cache: pages load from the internet again the first
  time; you stay signed in. It refills as you browse." "Shader cache: games
  build it again; the first launch may stutter." "Crash dumps: only needed
  to report a crash that already happened." Junk that refills within days
  says so, so the space won isn't oversold.
- **Junk of well-known apps**, like CCleaner's list of applications:
  Spotify, Discord, Slack, Teams, Zoom, VS Code, Steam, Adobe's media cache
  and others, each a tested rule with its own blurb. BleachBit's cleaners
  and `winapp2.ini` show where apps keep junk, but are checked rule by rule
  rather than imported, since one wrong rule deletes someone's data. An app
  that's running is skipped until it's closed.
- **The result afterwards:** what was freed, what was skipped (files in
  use) and what will refill.
- **Recoverable where it counts:** "check first" items go to the quarantine
  (phones and desktops get one, as servers have since 0.8). Caches and
  temporary files are deleted after the confirmation, which says so: their
  apps rebuild them, and in a quarantine they'd free nothing.
- **Reminders, never automatic cleaning:** an optional daily or weekly check
  says when junk passes a threshold ("2.4 GB of junk to review"). CCleaner's
  automatic cleaning is left out on purpose. On the Hub: each device's junk
  total, an alert above a threshold, and a request to clean, approved on the
  device.
- **Android:** the Free up space cards become the cleaner's categories, with
  old downloads, screen recordings and empty folders added. Other apps'
  private caches still have to be cleared one by one until Shizuku mode.
- **Left out on purpose:** registry cleaning (frees nothing and can break
  Windows), memory "boosters", and browser history, cookies and saved
  passwords (privacy rather than space, and cleaning them signs people out).
- **Three rules for building it:** every rule has a fixture test (a made-up
  folder it must find and one it must not); every item has a blurb and says
  whether it refills; an app that's running is skipped, and "check first"
  items are never ticked.
- Builds on the system and app cache rules (0.6), Android's Free up space
  (0.3) and the quarantine (0.8, everywhere from 0.9).

## Phase 7: Updates from inside the apps

A new release shows up in the app, and the app installs it itself where the
platform allows.

- **Android:** the app downloads the APK and hands it to Android's package
  installer, which asks the person (the `REQUEST_INSTALL_PACKAGES`
  permission; the same signing key, which `build-apk.sh` guarantees).
- **Windows and Linux:** the app downloads the archive, checks it, swaps its
  one executable and starts again. On Windows an unsigned executable gets
  SmartScreen's warning on every update and a signed one carries its
  reputation forward; an EV certificate no longer skips the warning, so
  either a code-signing certificate comes with the updater or the "Run
  anyway" click is documented.
- **The Unraid plugin** already updates through its `.plg` on the Plugins
  page.
- **The Hub** in Docker can't replace itself: it shows that a newer version
  exists and which devices run old versions (every device reports its app
  version already), and Unraid's Docker page or Watchtower does the pull.
- **Checking the download:** a sums file from the same server proves
  nothing, so `release.sh` signs `SHA256SUMS.txt` with a release key and the
  apps carry the public key (the Ed25519 code in `crates/proto`).
- **Privacy:** an update check is a request to GitHub, so it's opt-in on
  devices, or goes through the Hub, which checks once for everyone. A beta
  channel for the people who help test.

## Phase 8: Where the space goes

The question every tool's users ask and none answers in full: used space
that no file in the scan accounts for, on every platform.

- Done: ZFS snapshots, Windows shadow copies and restore points, Time
  Machine's local snapshots, docker.img on Unraid; in 0.11, Nextcloud's
  own data folder read on the server by the plugin (previews, versions,
  trash, uploads and appdata per user, with the retention settings that
  would have cleared them; whether the background job runs isn't in any
  file, so the report says what's past the settings and where to check),
  docker.img writers (which container writes inside its own layer and
  fills the image), macOS's purgeable space and other APFS snapshots, and
  btrfs snapshots (their exclusive size with quotas on, else a count).

## Traps to remember

- **Last-access times are unreliable** (Windows stops updating them on big
  volumes, Linux mounts with relatime or noatime, Android likewise). "Not
  opened in months" has to come from modification times and app signals,
  and say so.
- **Store policy:** Google Play limits All files access, `QUERY_ALL_PACKAGES`
  and Shizuku-style automation. DiskTree ships as an APK; a Play version
  would lose features.
- **Licenses:** DiskTree is GPL-3.0-or-later (decided 2026-10-12), so code
  and rules from GPL tools (QDirStat, WinDirStat, Filelight, BleachBit, SD
  Maid SE) may come in; ncdu, gdu, dua, Scrutiny and Czkawka's core are
  MIT. Slint's royalty-free license asks for attribution ("Made with
  Slint" in the About box).
- **Google's developer verification** for Android doesn't touch APKs people
  install directly until its global phase in 2027 (from September 2026 it
  applies to the big stores in four countries). Before then the package and
  its signing key get registered; a limited-distribution account covers 20
  devices at no cost, a full one costs $25.
- **Background work on Android** needs a Java or Kotlin class declared in
  the manifest; Rust can't register one alone. Periodic work every 15
  minutes is the shape for the agent; a data-sync foreground service is
  capped at 6 hours a day on Android 15.
- **Windows' own documents disagree** about Storage Sense's default for
  OneDrive files (never, or online-only after 30 days from Windows 11 22H2),
  so the cleaner reads the live setting rather than assuming.
- **The audit chain proves nothing on its own**: whoever can edit the
  database can rewrite the chain. Its head hash has to go somewhere else
  (the log, an alert channel).
- **An agent that runs as root or SYSTEM and can delete** raises the stakes:
  keep device approvals for every destructive action, and make every remote
  view read-only by default (JAM made SpaceObServer's web access read-only
  for the same reason).
- **Unraid's own prelude** (`local_prepend.php`, run before every page
  script) checks a POST's token and then removes it from `$_POST`. A
  plugin's PHP can't check it a second time; `unraid/tests/page_test.php`
  runs the page behind a copy of that prelude.
- **Android refuses plain `http://`** for apps targeting Android 9 and later
  unless the manifest allows it. `usesCleartextTraffic` does that until the
  app targets API 38, which ignores it; then it takes a network security
  config.
- **Android 17's local network permission**: an app targeting API 37 needs
  `ACCESS_LOCAL_NETWORK`, asked at run time, to reach a Hub on the LAN. Apps
  targeting 36 or lower get LAN access through `INTERNET`.
- **Apple refuses TLS server certificates valid for more than 825 days**,
  without the name in the subject alternative names, or without server
  authentication in the extended key usage, on macOS and iOS alike. The
  Hub's own certificate has to renew itself.
- **Installing the Hub's authority in a browser** makes it trusted for
  every site unless the browser honors its name constraints, and Chrome on
  Linux didn't for imported roots as of 2023. The authority is named for
  the Hub's address only, and whether each browser enforces that needs
  testing before the dashboard suggests installing it.
- **Extra rights on each platform**: an elevated Windows program doesn't see
  the user's mapped network drives (a separate sign-in session), System
  Volume Information stays closed even to administrators, and Windows'
  reserved storage is invisible to every program. macOS has no API to ask
  whether Full Disk Access is granted; reading a protected file is the
  test. Running a window as root on Linux is refused by Wayland sessions
  and bad practice anyway, so only the scan runs as root.
- **The first visit in a browser** to a generated certificate is trust on
  first use: an attacker on the network at that moment could stand in.
  Devices don't have that gap, since the pairing link carries the Hub's key.
- **Windows Storage Sense** already makes OneDrive files online-only after
  30 unopened days on C:. The Nextcloud work is worth it for verified copies
  and other drives, not for that toggle.

## Ideas that set it apart

- One map of where every copy of your data lives, across phones, PCs, the
  NAS and the cloud, including what has no copy at all.
- A "safe to free" score per item: has copies, can be rebuilt (caches, build
  output), not opened in months.
- What changed since the last scan, per device: new big files and folders
  that grew.
- Quarantine with restore everywhere, instead of permanent delete.
- Time-boxed, device-confirmed admin actions with a tamper-evident audit
  log.
- Docker and VM awareness on any Linux host, not only Unraid.
- Media libraries (Plex, Jellyfin, the *arr apps): show what's watched and
  old, and hand pruning to the tools built for it rather than duplicating
  them.

## Decided

- Phases 2b and 2c come before the Hub (done in 0.4), then 2d (0.5) and
  2e (0.6). The Hub (phase 3) is in 0.7, the Unraid plugin (phase 4), with
  the quarantine and the approver in its settings page, in 0.8.
- After 0.8.3 (decided 2026-10-12): 0.8.4 fixes, 0.8.5 the fixes found
  testing 0.8.4 on a real server and phone, 0.8.6 the rename to FreeSize,
  0.8.7 HTTPS by default, 0.8.8 the whole disk on every platform (both
  added 2026-10-13), 0.9 trust and updates (phase 7), 0.9.1 Community Applications,
  0.10 the junk cleaner (phase 6), 0.11 where the space goes (phase 8) and
  balancing, 0.12 Nextcloud read-only, 0.13 Nextcloud actions, 0.14 the
  phone in the background. See Next releases.
- Nothing that deletes ships on a platform before that platform has the
  quarantine.
- The name is FreeSize; the Android package id is
  `io.github.mcgeaverbeaver.freesize`, since the project has no domain.
- The license is GPL-3.0-or-later for everything.
- Nextcloud beside the plugin is the supported setup; elsewhere works
  read-only through WebDAV.
- The Hub is built as if reachable from the internet; whether it is stays
  the admin's choice.
- The Hub serves HTTPS by default with a certificate it makes itself;
  devices trust it because the pairing link vouches for the Hub. An admin's
  own certificate (environment variables) or a reverse proxy replace it
  (decided 2026-10-13).
- The Android app gets one small Java Worker class for background work; the
  logic stays in Rust.
- Screenless devices use a designated approver who can only be changed on
  the device (see Approvals).

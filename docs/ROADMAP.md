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
  - the basket: collect from any view, review once, delete all or move all
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

Left over:
- From 2b/2c: old downloads and screen recordings as their own groups
  (phase 6's junk cleaner picks them up). Docker's build cache is cleaned
  on Unraid since 0.8.
- From 2d: an Apps bubble view on desktops (an app's install folder plus its
  AppData/Library/cache folders, gathered across drives); Epic and GOG game
  artwork.
- From 3, each held back for a reason:
  - **The Hub on Windows** as a service from the installer, with a firewall
    rule for private networks: needs the installer (NSIS) and a Windows
    service wrapper, tested on real Windows. The Hub runs in Docker Desktop
    meanwhile.
  - **The phone in the background**: the Android app only reports and gets
    prompts while it's open (an alert nudges you to open it). A small Java
    part with WorkManager fixes that, together with Shizuku below.
  - **More actions**: removing packages (apt, dnf, Flatpak, Snap, winget,
    Homebrew), Docker cleanup outside Unraid and making OneDrive/Nextcloud files
    online-only. Each needs its own checks of what's safe; deleting files,
    cleaning junk and uninstalling Android apps come first.
  - **Scanning over SSH** without an agent: uploading `ncdu -o` files covers
    it for now, without the Hub holding SSH keys.
  - **Mass-change warnings** (thousands of files rewritten, a new extension
    everywhere): needs file-level changes between scans, which the summaries
    don't keep.
  - **Email alerts and scheduled digest reports**: ntfy, Gotify, Pushover
    and webhooks (which can reach email through Home Assistant or n8n)
    cover alerts for now.
  - **Disk health from Scrutiny**, **S3/B2/MinIO buckets**, **per-user
    totals** for shares and Nextcloud, **tags and keep marks** on what's
    found, and **MQTT discovery** for Home Assistant (the REST sensor works
    meanwhile).
  - **An MCP server** for AI assistants: read-only first, with actions
    still approved on the device.
- From 2e, each held back for a reason:
  - **Shizuku mode on Android** (batch cache clearing and `Android/data`
    through an unrooted phone's helper): needs a Java part in the APK, which
    the Hub's background agent needs too, so the two come together.
    Sideloaded or F-Droid builds only; Play restricts it.
  - **Shrinking photos** (SD Maid SE's Squeezer): rewrites people's photos,
    so it waits for a quarantine with restore on phones and desktops (servers
    have one since 0.8) to keep the originals until the user is happy.
  - **Blurry photos**: a sharpness score per photo, on top of the similar
    photos finder, to pick the best of a group.
  - **NTFS compression in place** and **fast Windows scans** from the
    Master File Table and change journal (as WizTree and SpaceObServer do):
    both need administrator rights and testing on real Windows machines
    with NTFS drives, which the Linux build machine can't stand in for.
  - **btrfs snapshots and APFS purgeable space** in the hidden space
    explanation (ZFS, Windows shadow copies and Time Machine are done).
  - **Leftovers of uninstalled programs** in AppData and `~/Library`:
    matching folders to installed programs is reliable for the icons but not
    yet reliable enough to call a folder abandoned. Phase 6's junk cleaner
    picks it up once it is.
- From 4, each held back for a reason:
  - **Container logs, writable layers and orphan volumes** are only shown:
    Docker keeps writing to a running container's log, a layer is part of
    the container, and a volume's data can't go to the quarantine without
    Docker losing track of it. The Docker tab handles them.
  - **Community Applications**: the plugin installs from its `.plg` URL
    until it's submitted there; the repository is public since 0.8.3, so
    it can be.
  - **Unraid's own notifications** for full disks: the Hub's alerts cover
    it once paired.

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
  to the WireGuard address keeps it off the internet. HTTPS through a reverse
  proxy with a real certificate is recommended even inside the VPN.

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

| Platform | Reports | Can do (after approval) |
|---|---|---|
| Android | Everything the app shows; only while open until the app gets a small Java part for background work | Delete junk and trash; uninstall (Android still shows its own confirmation) |
| Linux | Disks, folders, apt/dnf/flatpak/snap, Docker | Remove packages, delete files (via polkit/sudo per action) |
| Windows | Disks, installed programs | `winget` uninstall, delete files, make OneDrive/Nextcloud files online-only |
| macOS | Disks, apps, Homebrew | Move apps to Trash, `brew uninstall` (needs a Mac to build) |

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

- **Nextcloud server app:** exports each user's file index (path, size,
  modification time, checksum) and reports server-side space: trash bin,
  file versions, previews.
- **Desktop agents** read the Nextcloud client's sync folders and sync
  journals: which files are synced, which are kept on the device ("always
  available offline") and which are online-only placeholders.
- **Android:** the Nextcloud app's offline copies (`Android/media`) and
  camera photos already auto-uploaded.
- **Matching** local files to copies on Nextcloud and on other devices:
  name, size and date find candidates, a content hash confirms them. Shown as
  "verified copy" or "likely copy".
- Nextcloud often leaves its checksum column empty, so the server app
  hashes files itself and caches the result by file id, size and
  modification time.
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
- Builds on the system and app cache rules (0.6), Android's Free up space
  (0.3) and the quarantine (0.8).

## Traps to remember

- **Last-access times are unreliable** (Windows stops updating them on big
  volumes, Linux mounts with relatime or noatime, Android likewise). "Not
  opened in months" has to come from modification times and app signals,
  and say so.
- **Store policy:** Google Play limits All files access, `QUERY_ALL_PACKAGES`
  and Shizuku-style automation. DiskTree ships as an APK; a Play version
  would lose features.
- **Licenses:** code from GPL tools (QDirStat, WinDirStat, Filelight) can't
  be copied in unless DiskTree is GPL; ncdu, gdu, dua and Scrutiny are MIT.
  Slint's royalty-free license asks for attribution. DiskTree's own license
  is still to be chosen.
- **An agent that runs as root or SYSTEM and can delete** raises the stakes:
  keep device approvals for every destructive action, and make every remote
  view read-only by default (JAM made SpaceObServer's web access read-only
  for the same reason).
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
  the quarantine and the approver in its settings page, in 0.8. Nextcloud
  (phase 5) is next.
- Screenless devices use a designated approver who can only be changed on
  the device (see Approvals).

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
  emptied), with a retention period and one-click restore.
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
- 0.5 (phase 2d): icons on the treemap (app, game or kind of content),
  the bubble view with zoom and a tappable path, green outlines on
  rebuildable folders, app and game icons for folders on every platform
  (Android packages, Steam, Linux desktop entries, Windows programs, macOS
  bundles), and app groups as bubbles on Android with each app's parts.

Left over:
- From 2b/2c: old downloads and screen recordings as their own groups, and
  Docker build cache (part of the Docker work in phases 3 and 4).
- From 2d: an Apps bubble view on desktops (an app's install folder plus its
  AppData/Library/cache folders, gathered across drives); Epic and GOG game
  artwork; a "since last scan" mode (bubbles that grew get a ring and a
  "+2.1 GB" badge), which needs the folder history below.

## Phase 2e: next on every device

Found by comparing with SpaceObServer and TreeSize Professional (JAM
Software), WizTree, WinDirStat, QDirStat, DaisyDisk, SD Maid SE, Files by
Google, Scrutiny, Diskover and unBALANCE.

- **Folder history and diffs.** Keep folder totals per scan (not every
  file) and compare any two scans: what grew, what's new, what's gone. Feeds
  "since last scan" and the Hub's history.
- **Export** of every list (CSV, JSON, HTML) and `disktree --json`, for
  spreadsheets, scripts and Home Assistant. Import and export of scan files
  (an ncdu-compatible JSON), so a box without DiskTree can be scanned with
  `ncdu -o` and viewed here.
- **Move instead of delete.** Move or copy a selection elsewhere, keeping
  its folders, with a manifest to put it back: phone videos to an Unraid
  share over WireGuard, old PC data to the array. Optionally compress in
  place (NTFS) instead.
- **File age and size histograms** (by year modified, by size band), from
  the scan data already there.
- **Hidden space**: say how much is in ZFS/btrfs snapshots, Windows shadow
  copies and restore points, and APFS purgeable space, which explains why
  files add up to less than the used space.
- **Windows junk**: `Windows.old`, update and Delivery Optimization caches,
  hibernation file, browser and shader caches; leftovers of uninstalled
  programs in AppData (and Library on macOS).
- **A collect-then-act basket** across views, so a cleanup can be put
  together from several places and reviewed once.
- **Android**: a Shizuku mode (an unrooted phone's helper for batch cache
  clearing and `Android/data`; sideloaded or F-Droid builds only, Play
  restricts it), similar and blurry photos, old screenshots, chat-app media,
  and shrinking photos (SD Maid SE's Squeezer).
- **Fast Windows scans** from the NTFS Master File Table and change journal
  (as WizTree and SpaceObServer do), an optional admin mode.

## Phase 3: DiskTree Hub (dashboard for all devices)

One small Rust server with a web UI and SQLite.

**Running it**
- Docker image for amd64 and arm64: runs on Unraid (Community Apps
  template), any Linux host, Synology, or Docker Desktop.
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
  automatic. The quarantine lives on the same disk (`/mnt/diskN/.disktree`),
  or the "instant" move becomes a copy between disks.
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

- Phases 2b and 2c come before the Hub (done in 0.4), then 2d (0.5).
- Screenless devices use a designated approver who can only be changed on
  the device (see Approvals).

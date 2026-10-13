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
- 0.8.4: the Hub signs people in through Authentik providers without a
  signing key (HS256 with the client secret). Emptying the quarantine and
  Docker cleanup are action kinds of their own that no grant covers. The
  Unraid plugin works out what's protected when a request arrives, not at
  the last round. A scanned folder that is a link can't lead a delete out.
  The Hub refuses paths that aren't in the device's newest scan. Pairing
  over plain `http://` to another machine warns. A damaged device key is
  refused rather than replaced. The README reads the client secret from a
  file and says what a device sends.

Left over and planned (the release it goes in is under Next releases):
- **A quarantine on phones and desktops**, with one Put back for everything,
  the basket's own undo folded in (0.9). Shrinking photos (SD Maid SE's
  Squeezer) waits for it, since it rewrites people's photos.
- **The phone in the background**: the Android app only reports and gets
  prompts while it's open (an alert nudges you to open it). One small Java
  Worker class, compiled by `build-apk.sh`, scheduled every 15 minutes and
  calling into Rust, fixes that; **Shizuku mode** (batch cache clearing and
  `Android/data` through an unrooted phone's helper) needs the same Java part
  and follows it. Sideloaded builds only; Play restricts it (0.14).
- **Community Applications** for the plugin and the Hub template (0.9.1).
- **btrfs snapshots, APFS snapshots and purgeable space** in the hidden space
  explanation, and Nextcloud's own data folder (phase 8, 0.11).
- From 2b/2c: old downloads and screen recordings as their own groups, and
  **leftovers of uninstalled programs** in AppData and `~/Library` once
  matching folders to programs is reliable enough to call one abandoned
  (phase 6, 0.10).
- **Making Nextcloud files online-only** on Windows (phase 5, 0.13).

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

## Next releases

Each release is small enough to ship in a few weeks, and each says what
"done" means. Trust and the quarantine come before any new way to delete;
the junk cleaner before Nextcloud; Nextcloud in two halves so the read-only
half ships even if an action stalls on a device nobody has.

- **0.8.5, the rename to FreeSize.** Crates, binaries (`freesize`,
  `freesize-gui`, `freesize-hub`), the Docker image, the plugin
  (`freesize.plg`, migrating `/boot/config/plugins/disktree`), the data
  folders (migrated on first start), the pairing link scheme (both accepted
  for one release), the Android package id
  (`io.github.mcgeaverbeaver.freesize`; a new id is a new app to Android, so
  the old one is uninstalled once; the signing key stays). The wire names
  inside the protocol stay as they are. Done when a 0.8.4 plugin, Hub,
  Windows app and phone each upgrade with pairing, settings, history and
  quarantine intact.
- **0.9, trust and updates.** The license (GPL-3.0-or-later) and a
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
- **0.9.1, out in the open.** Community Applications submission for the
  plugin and the Hub template (the license, a `ca_profile.xml`, the plugin
  wrapper XML pointing at the raw `.plg`, a support thread on the forum);
  the README split into a user page and a developer page. Done when both
  appear in the Apps tab on a clean Unraid 7.3 install and install from
  there.
- **0.10, the junk cleaner (phase 6).** As described there, with three
  constraints: a fixture test per rule, a blurb and a "refills" flag per
  item, running apps skipped; "check first" items go to the quarantine and
  are never ticked. Done when, on a Windows machine, a Linux box and a
  phone, the cleaner lists nothing the system's own tool would refuse,
  frees what it says within a few percent, and the result screen shows
  what was skipped.
- **0.11, where the space goes, and balancing (phase 8 and Unraid).**
  Nextcloud's data folder read on the server (previews, versions, trash,
  appdata, per user, with the retention settings that would have cleared
  them); docker.img writers named per container; the mover keeps sparse
  files sparse and copies extended attributes, cleans stale partial files,
  and offers "empty the quarantine once every copy checked out" as one more
  tick; the empty-a-disk and gather-a-share presets as plans of folder
  moves; a round drops each disk's tree once it's used; progress per file.
  Done when the Nextcloud numbers match `du` within 1%, a stopped VM's
  sparse vdisk moves with its holes kept, and a round on a big array logs
  its peak memory under a set number.
- **0.12, copies, read only (phase 5, first half).** Connect Nextcloud
  through Login Flow v2; file ids, sizes and checksums over WebDAV; each
  desktop client's sync journal; the Android app's folder; the Hub joins
  on file id and shows "kept on N devices" per folder, with the "only copy"
  risk report. Done when the count is right for 20 files checked by hand
  on three devices and the risk report lists a file kept on one device only.
- **0.13, copies, actions (phase 5, second half).** Online-only on
  Windows, uploaded camera photos to the phone's quarantine, moving a
  folder into the sync folder, creating folders on Nextcloud, External
  storage on Unraid. macOS and Linux show and point to the client. Done
  when a 10 GB folder on a Windows PC becomes online-only after the next
  sync with the files still listed, comes back on demand, and nothing is
  freed before the hash matches.
- **0.14, the phone in the background.** The Java Worker, approvals as
  notifications, then Shizuku. Done when a phone with the app closed
  reports within 15 minutes of a scan and shows an approval within 15
  minutes of a request, over a day, without the battery screen listing
  FreeSize above the usual apps.

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
| Android | Everything the app shows; only while open until 0.14 | Delete (permanently, until the phone's quarantine in 0.9); clean junk; uninstall (Android still shows its own confirmation) |
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
- **Desktop agents** read the client's sync journal, `.sync_<12 hex>.db`
  next to each sync folder: `metadata.fileid`, `contentChecksum` and its
  type, and the `flags` table's pin state (always local, online-only,
  inherited), which is what the person asked for, not what is on disk; what
  is on disk comes from the placeholder itself.
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
  Machine's local snapshots, docker.img on Unraid.
- **Nextcloud's own data folder**, read on the server by the plugin:
  previews (`appdata_*/preview`), versions, trash and appdata, per user,
  with the retention settings that would have cleared them and whether the
  background job that clears them is running.
- **docker.img writers:** which container writes inside its own layer and
  fills the image.
- **macOS:** purgeable space and System Data, and APFS snapshots.
- **btrfs snapshots.**

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
- After 0.8.3 (decided 2026-10-12): 0.8.4 fixes, 0.8.5 the rename to
  FreeSize, 0.9 trust and updates (phase 7), 0.9.1 Community Applications,
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
- The Android app gets one small Java Worker class for background work; the
  logic stays in Rust.
- Screenless devices use a designated approver who can only be changed on
  the device (see Approvals).

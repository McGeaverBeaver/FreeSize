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

Left over from 2b/2c: old downloads and screen recordings as their own
groups, and Docker build cache (part of the Docker work in phases 3 and 4).

## Phase 2d: see it at a glance (every platform)

Make the biggest things recognisable before reading a single label.

**Icons on the treemap**
- The largest blocks (those big enough to hold one) show an icon: the app's
  own icon when the folder belongs to an app, otherwise one for the kind of
  content (videos, photos, music, archives, disk images, games, code, caches).
- Icons and labels scale with the block; tiny blocks stay plain.

**Bubble view**
- A second way to draw the same folder: one circle per item, its area
  proportional to its size, the biggest in the middle, each showing its icon
  (Steam's icon on the Steam bubble, each game's artwork on the game).
- Tap or click a bubble to zoom into it: Steam opens into its games,
  screenshots, shader cache and so on, and those open further, down to files.
  A breadcrumb trail and Back/Up zoom out again. The zoom animates.
- Small items are folded into one "Other" bubble so it stays readable.
- Long-press or hover: size, share of the parent, last change, and the usual
  actions (open, show in folder, delete after confirming).
- One toggle switches between Treemap and Bubbles; both follow the folder
  tree's selection.
- An **Apps** bubble view on the start screen: every app as one bubble sized
  by everything it uses. On Android that's code + data + cache + the files it
  saved; on desktops the install folder plus its data and cache folders
  (AppData, Library, `~/.cache`, `~/.var/app`), gathered from wherever they
  are, even across drives. Zooming into an app shows those parts.

**Where icons and owners come from**

| Platform | Apps | Games and other content |
|---|---|---|
| Android | `PackageManager` icons; `Android/data`, `media` and `obb` folders by package name | MediaStore file types |
| Windows | Installed programs from the registry (install folder, `DisplayIcon`); icons from the shell | Steam libraries: each `appmanifest_*.acf` names a game folder, and Steam's library cache has the artwork. Epic and GOG manifests likewise |
| macOS | `.app` bundles, `~/Library/Containers/<bundle id>` | Steam as on Windows |
| Linux | `.desktop` files and the icon theme; Flatpak (`~/.var/app/<id>`) and Snap | Steam as on Windows |

Icons are cached on disk after the first load, and only the largest visible
items load theirs.

**Extras that fit here**
- Colour or outline by what can go (rebuildable, duplicate, untouched for a
  year), so the bubbles double as a cleanup map.
- "Since last scan" mode: bubbles that grew get a ring and a "+2.1 GB" badge.
- The Hub (phase 3) reuses it: one bubble per device, then its apps and
  folders.

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

**Also**
- Alerts: disk over a threshold, fast growth, forecasted full.
- Metrics for Prometheus/Grafana and Home Assistant sensors.
- An MCP server so an AI assistant can answer "what's filling the array?" and
  propose actions, which still need the device's approval.

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
- **Deleting:** preview and browse first, quarantine share with restore,
  nothing automatic.
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

- Phases 2b and 2c come before the Hub (done in 0.4).
- Screenless devices use a designated approver who can only be changed on
  the device (see Approvals).

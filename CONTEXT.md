# FreeSize

FreeSize shows people where their disk space went and helps them win it back
safely, on their own devices and, through the Hub, on every device they own.
Nothing is ever removed without the person seeing the exact paths first.

## Language

### Looking at space

**Scan**:
One reading of a folder or drive and everything under it, with sizes.
_Avoid_: index, crawl

**Scanned folder**:
The folder a scan started from; paths in requests and moves are given
relative to it.
_Avoid_: root (in user-facing text)

**Saved scan**:
A scan opened from a file (FreeSize's or ncdu's), read-only because its files
may be on another machine.
_Avoid_: imported scan, offline scan

**Fuller scan**:
A scan run with more rights (administrator or root) to read the folders an
ordinary scan couldn't.
_Avoid_: elevated scan, admin scan

**Changes**:
What grew, appeared, shrank or went away between two scans of the same
folder.
_Avoid_: diff, delta

**Hidden space**:
Used space that no file accounts for: snapshots, restore points, reserved and
purgeable space, unreadable folders. Shown as "Not in any file".
_Avoid_: missing space, unaccounted space

### Finding what can go

**Find**:
One thing a look turned up that could free space, such as a rebuildable
folder, a duplicate set or a junk rule's paths. Suggested only, never acted on
by itself.
_Avoid_: issue, problem, recommendation

**Card**:
A summary of one kind of find, with how much it could free, that opens to a
page listing them.
_Avoid_: tile, panel

**Rebuildable folder**:
A folder a tool makes again when needed (build output, installed packages),
shown with the command that brings it back.
_Avoid_: build artifacts, temp folder

**Duplicate set**:
Files with identical contents. One copy in each set is the **kept copy**; the
rest are **extra copies**.
_Avoid_: duplicate group, clones

**Similar photos**:
A set of photos that look alike but aren't identical, such as bursts and
resized copies.
_Avoid_: duplicate photos

**Review** (of a folder):
A page listing one folder's files by kind (Photos, Documents, Videos, Worth
a look, Everything else), to look through before deleting it. **Worth a
look** files are the ones people most often miss; **Only here** marks a
file not found elsewhere in the scan by content.
_Avoid_: inspect, audit, folder report

**Junk**:
Space the system or apps can do without that sits in known places, found
without a scan.
_Avoid_: garbage, cruft

**Junk rule**:
One known place junk collects (a browser's cache, old installers), with what
cleaning it means and whether it refills.
_Avoid_: junk kind, junk category (a category groups rules)

**Safety**:
How a find may be removed, one scale everywhere (junk rules, Docker and VM
finds): **Safe** (deleted, comes back by itself), **Check first** (never
ticked by default; files go to the quarantine, Docker images are removed
through Docker), **Use the tool** (never ticked;
the system's own tool removes it) or **Protected** (never offered).
_Avoid_: risk level, Check (say Check first)

**Leftovers**:
Settings and caches a program or app left behind after it was uninstalled,
offered only when FreeSize is sure whose they were.
_Avoid_: orphans, remnants

**App group**:
On a phone, a list of apps sorted by one way of winning space back (Biggest
caches, Not opened in 3 months, Not from an app store).
_Avoid_: app category, app filter

### Removing safely

**Highlight**:
Click or tap a row to show it in the tree and the bar below, without adding
it to the selection.
_Avoid_: select (that adds to the selection)

**Tick**:
Mark a find to be acted on. Ticking adds it to the selection.
_Avoid_: check, mark

**Selection**:
Everything added to it, by ticking or with the Select button, anywhere, reviewed in one list before it is
deleted or moved together. Buttons that add say **Select …**, the ones that
take back out **Unselect …**, and the list opens with **Review selection…**.
_Avoid_: basket, collection, cart

**Quarantine**:
Where deleted items are moved aside, on the same disk, until they are put
back or emptied.
_Avoid_: trash, recycle bin (those are the system's own)

**Batch**:
The items that went to the quarantine together, put back or emptied as one.
_Avoid_: group, set

**Delete**:
Ask for an item to go; it moves to the quarantine. (Safe junk is the
exception: it is removed outright, since quarantining it would free nothing.)
_Avoid_: remove, trash

**Put back**:
Return a batch from the quarantine, or the last move, to where it was.
_Avoid_: restore, undo

**Empty**:
Remove quarantined batches for good, the only step that frees their space.
_Avoid_: purge, delete permanently

**Clean**:
Remove what the ticked junk rules found, each according to its safety.
_Avoid_: sweep, wipe

**Move**:
Carry selected items to another folder or disk, keeping their folders below
the scanned folder, with nothing removed until the copy checks out.
_Avoid_: transfer, relocate

**Protected**:
Something never offered for removal, whoever asks: system files, what
containers and VMs use, files open right now, FreeSize's own files.
_Avoid_: locked, in use (in use is one reason for it)

### The Hub

**Hub**:
The self-hosted dashboard that collects every paired device's storage and
sends requests to them.
_Avoid_: server, console

**Device**:
A phone, PC or server paired with the Hub, or one the Hub only knows from
uploaded scans.
_Avoid_: client, node, host

**Agent**:
The command line running unattended as a device, on a server.
_Avoid_: daemon, service (in user-facing text)

**Pairing**:
Connecting a device to a Hub once, with a short-lived code, after which they
trust each other's keys.
_Avoid_: enrolment, registration

**Report**:
What a device sends the Hub about itself: disks, its biggest folders and
files, apps, junk, quarantine, synced folders. Never file contents.
_Avoid_: upload, telemetry

**Admin**:
A person signed in to the Hub who may pair devices, send requests and change
settings.

**Viewer**:
A person signed in to the Hub who may look but change nothing.

**Device group**:
Devices that share settings on the Hub (the grant lengths on offer, who may
see them).
_Avoid_: fleet, tag

**Request**:
An admin asking a device to do one thing (delete, clean, uninstall, put back,
empty…), shown on the device with every path before anything happens.
_Avoid_: command, job, task

**Approval**:
The device's yes to one request, given there: by someone at the device, by
its approver, or by a running grant.
_Avoid_: confirmation, consent

**Grant**:
Permission an admin gets on a device to make the same kind of request again
without asking, for a set number of minutes. Never covers what can't be put
back.
_Avoid_: session, token, approval window

**Approver**:
The phone or PC that approves a server's requests, since nobody sits at the
server. Chosen on the server only.
_Avoid_: owner, approving device

**Alert**:
A message the Hub sends out when a disk fills, grows fast, a device goes
silent or junk piles up.
_Avoid_: notification (that is the phone's)

**Audit log**:
The Hub's record of every sign-in, pairing, request, answer and setting
change, chained so a change or gap shows.
_Avoid_: history, activity log

### Copies

**Synced folder**:
A folder the Nextcloud client keeps in step with the server.
_Avoid_: sync root, Nextcloud folder

**Stored**:
Held on the device's own disk, as opposed to **online-only** (listed but
fetched when opened).
_Avoid_: local, offline, pinned

**Kept on**:
The devices that store a copy of a folder or file ("kept on 3 devices").
_Avoid_: replicated, backed up

**Only one copy**:
Something that exists in one place only, which a lost device or failed server
would take with it.
_Avoid_: unbacked, at risk

**Suggestion**:
A request the Copies page proposes to free or protect space (free a stored
copy, move a folder into a synced folder, add a share as external storage).
Edit changes where a moved folder goes; Ignore sets a suggestion aside
under Ignored, where Show again brings it back.
_Avoid_: recommendation, action, dismiss, snooze

**Nextcloud app**:
The FreeSize app inside Nextcloud (0.20), paired with the Hub as the device
"Nextcloud": it reads the covered users' real files on the server and moves
what is approved into their trash bin.
_Avoid_: plugin, Nextcloud integration, connector

**Extra copy**:
A file whose contents are the same, byte for byte, as an older file's (by a
SHA-256 of the whole file). The oldest of the set is kept; the rest are the
extra copies.
_Avoid_: duplicate (alone), similar file

**Covered user**:
A Nextcloud user the admin chose for the Nextcloud app to report on.
_Avoid_: monitored user, tracked account

# Security

FreeSize looks at every file on a device and can delete some of them, and
the Hub can be reached from the internet. This page says what the software
is built to withstand, what leaves a device, and how to report a problem.

## Reporting a problem

Open a [private security advisory](https://github.com/McGeaverBeaver/FreeSize/security/advisories/new)
on GitHub, or an issue if the problem is already public. Say which version,
which part (app, command line, agent, Hub, Unraid plugin) and what you saw.
There is no bug bounty; a fix and a thank-you in the release notes are what's
on offer. Problems in a dependency are best reported to that project too.

## What leaves a device

Nothing, unless you connect a Hub or turn on update checks.

- **The app and the command line** read the disk and write to their own data
  folder (scan history, settings, the quarantine's manifests). They make no
  network requests.
- **A paired device** sends its Hub, over HTTPS: its name, platform, OS and
  app version; its disks' sizes; after each scan the sizes of its biggest
  folders (up to 10,000), the paths and sizes of its biggest files (500),
  file types, and the paths of folders it couldn't read; on Android the
  list of installed apps with their sizes and when they were last used; on
  Unraid the array, shares, Docker and VMs; and what its quarantine holds.
  File contents are never sent. Everything is signed by the device's key.
- **Update checks** (0.9, off unless turned on) ask GitHub for the latest
  release, which tells GitHub your address and the app's version. A Hub can
  check once for all its devices instead.
- **Alerts** go to the channels an admin sets up (ntfy, Gotify, webhooks,
  Home Assistant), with the alert's text and the Hub's name.

## Threat model

What FreeSize defends against:

- **Someone on the network who can read or alter traffic.** The Hub serves
  HTTPS with a certificate it makes itself (or the admin's); devices pin
  the authority they learned when pairing, and the pairing link carries the
  Hub's key fingerprint, so a Hub that isn't the one the link named is
  refused. Every device request is signed with the device's Ed25519 key
  and carries a timestamp and a nonce; every command from the Hub is signed
  with the Hub's key. Replays and edits are refused.
- **Someone who guesses at the Hub.** Pairing codes are 60 random bits,
  stored hashed, good for 10 minutes and one use; wrong guesses are limited
  per address and in total. Sign-in goes through OpenID Connect (Authentik)
  with PKCE, state and nonce; sessions are random tokens stored hashed in
  HttpOnly cookies, and changes need a CSRF token. Sign-in attempts are
  limited per address as well.
- **A compromised Hub or an admin overreaching.** A Hub can only ask. What
  it asks for is approved on the device itself (or by the device's
  designated approver, which the Hub can't change), for that request or for
  a few minutes; deletes may only reach scanned folders; emptying the
  quarantine and removing Docker images are asked for every time. Protected
  paths are never offered. Deleting moves items to a quarantine on the same
  disk, from which they can be put back.
- **Someone editing the record.** The Hub's audit log is a hash chain; every
  entry carries the hash of the one before, `/api/audit/verify` checks it,
  and the head hash goes to the Hub's log and into alerts so there's an
  anchor outside the database.
- **A tampered download.** Release files come with `SHA256SUMS.txt`, signed
  with the release key; the apps check the signature before installing an
  update, and refuse a sums file that doesn't verify.

What it doesn't defend against:

- **Root or an administrator on the device itself.** The device's key, its
  data folder and its quarantine are readable by anyone with that access.
- **A compromised identity provider.** Whoever Authentik says is an admin is
  an admin.
- **The Hub's host.** The database holds every device's reports in the
  clear; protect the host and its backups accordingly.
- **Loss of the Android signing key or the release key.** Updates then stop
  until a new key is distributed by hand; the keys never leave the owner's
  build machine.

## Supported versions

The latest release. Older releases get no fixes; update instead.

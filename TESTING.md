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
- [ ] Pairs from the QR code; a delete from the Hub is approved on the
      phone.
- [ ] Updates: a newer release is offered and opens for installing.

## Hub

- [ ] Starts fresh: makes its key and certificate, the dashboard signs in
      through Authentik over HTTPS.
- [ ] Upgrades from the previous version: devices, scans, alerts and the
      audit log are all there; `/api/audit/verify` passes.
- [ ] A device behind the Hub's version is flagged; the Hub says when a
      newer release exists (with the update check on).
- [ ] Alerts reach a channel; the message carries the audit log's head.
- [ ] A wrong pairing code ten times from one address is refused for a
      minute; other addresses still pair.

## Unraid plugin

- [ ] Installs from the release's `freesize.plg` and updates from the
      previous version with its settings, pairing and quarantine intact.
- [ ] The page shows the array, shares, Docker's findings; a move between
      disks runs and the log is clean.

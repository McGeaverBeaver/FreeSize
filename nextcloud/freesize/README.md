# FreeSize for Nextcloud

Pairs Nextcloud with a [FreeSize](https://github.com/McGeaverBeaver/freesize)
Hub, so the Hub shows what could be cleaned up in the files of the users
the admin chooses: the biggest files, files not changed in a year, the same
file twice (by a SHA-256 of its contents, read on the server), and what the
trash bin and old versions take. Nothing is deleted by itself: each delete
is approved on the approver's phone and goes to the user's trash bin.

Install: unpack the release's `freesize-nextcloud-<version>.tar.gz` into
`custom_apps`, run `occ app:enable freesize`, and pair under Administration
settings → FreeSize. See "Cleaning up Nextcloud" in the main README.

Update: under Administration settings → FreeSize → Updates, or with `occ
freesize:update` (`--check` only looks, `--file FILE` installs a tarball);
releases are checked against the release key the app carries. By hand:
unpack the new tarball over the app's folder and run `occ upgrade`.

License: AGPL-3.0-or-later.

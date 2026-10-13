# Release signing

`scripts/release.sh` writes `SHA256SUMS.txt` for a release's files and signs
it with the release key, so the apps can tell a release that came from the
build machine from one that didn't (GitHub serves the sums file and the
files from the same place, so the file alone proves nothing).

Once, on the build machine:

```sh
freesize release key ~/.freesize/release-key   # prints the public half
```

Paste the public half into `public-key.txt` here and commit it. From then
on `release.sh` signs every release (`SHA256SUMS.txt.sig` beside the sums),
and apps built from the repository check the signature before installing
an update. `freesize release verify SHA256SUMS.txt` checks a download by
hand with the key in this folder.

The private key never leaves `~/.freesize/release-key` (mode 0600). If it
is lost, make a new one, commit the new public half, and the next release
has to be installed by hand once; if it leaks, the same, at once.

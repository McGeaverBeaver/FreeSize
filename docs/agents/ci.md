# CI runs: newest wins, tags always finish

The rule for every GitHub Actions workflow in `.github/workflows/`, new ones
included (decided 2026-11-27):

1. **A newer run cancels an older one still going** for the same workflow
   and branch or pull request. Each workflow has, at the top level:

   ```yaml
   concurrency:
     group: ${{ github.workflow }}-${{ github.ref }}
     cancel-in-progress: ${{ !startsWith(github.ref, 'refs/tags/') }}
   ```

   Add to the group whatever keeps runs that do different jobs apart. In
   `ci.yml`, hand-started runs (which build the APK and the Unraid plugin)
   get `-dispatch` and pushes and pull requests `-auto`, so a push never
   cancels an APK build.

2. **Tag and release builds are never cancelled**: every version tag
   finishes and publishes. The `startsWith(github.ref, 'refs/tags/')` above
   does it. (No workflow runs on tags today; releases are built on the
   build machine by `scripts/release.sh`.)

3. **Runs that publish are never cancelled, and only the newest commit
   publishes.** A run that pushes something (an image tag, a release file)
   goes in a group of its own with `cancel-in-progress: false`, so it is
   never stopped halfway through a push; GitHub keeps only the newest run
   waiting behind it. Right before pushing, it checks that its commit is
   still the branch's head (`git ls-remote origin "$GITHUB_REF"`) and,
   if not, pushes nothing unless the newer commit builds the same thing.
   It also checks that what it would replace hasn't moved since it started.
   See the Push step of `hub-image.yml`. `latest` must never point at older
   code than a run that finished: `scripts/release.sh` also leaves `latest`
   alone when it already holds a newer version.

4. **Every run builds the whole tree at its own commit.** Nothing builds on
   an earlier run's output, so a cancelled run loses nothing: the newer run
   has all of its commits. `Swatinem/rust-cache` only speeds up the build
   (cargo rebuilds what changed, and a cancelled run saves no cache), and
   uploaded artifacts are each run's own full build. Keep it that way: no
   workflow may publish partial or incremental artifacts, or depend on an
   artifact of another run.

If a platform without `concurrency` ever runs these workflows (an old
Gitea), the first step of each job exits early when `github.sha` is no
longer the branch's head.

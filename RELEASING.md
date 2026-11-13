# Releasing md-tex

A release is a GitHub release made by `.github/workflows/release.yml` when a `vX.Y.Z` tag is pushed. This file is for the maintainer. Users install from the README.

## What the workflow does

1. Checks that the tag matches `version` in the root `Cargo.toml` and that `CHANGELOG.md` has a section for it (`tools/changelog-notes.sh` prints that section).
2. Runs the CI checks (`ci.yml`) on Linux and macOS.
3. Writes `THIRD-PARTY-LICENSES.md` with cargo-about (`tools/third-party-licenses.sh`). It fails if a dependency's license is not on the list in `tools/licenses/about.toml`.
4. macOS (`macos-15`, arm64): builds the app with `tools/bundle-macos.sh`, checks `--version`, the license files and the ad hoc signature, and zips it.
5. Linux (`ubuntu-22.04`, for an older glibc): builds the tarball and AppImage with `tools/package-linux.sh`, then installs and runs the tarball in clean Ubuntu 22.04 and 24.04 containers.
6. Collects the files with their `SHA256SUMS`, and on a tag publishes the release with the changelog section as its notes.

The workflows need no secrets. Only the `publish` job can write to the repository.

## Dry run

Run the workflow by hand: Actions > Release > Run workflow, or `gh workflow run release.yml --ref main`. A manual run does everything except publish, and leaves the files as the workflow artifact `release` (`gh run download <run-id> --name release`).

## Cut a release

1. Set `version` in `[workspace.package]` in `Cargo.toml` and run `cargo check` so `Cargo.lock` records it.
2. Add a `## X.Y.Z` section at the top of `CHANGELOG.md`.
3. Commit, push, and wait for CI to pass.
4. Do a dry run (above) and try the files.
5. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.

## Local packaging

- `tools/third-party-licenses.sh` needs cargo-about: `cargo install cargo-about --locked --features cli`.
- `tools/package-linux.sh` builds `target/linux/md-tex-<version>-<arch>.tar.gz` and an AppImage. It downloads a pinned appimagetool, checked against its SHA-256, into `~/.cache/md-tex-packaging`.
- `tools/bundle-macos.sh` builds `target/release/md-tex.app`, signed ad hoc. The app is not notarized, so a downloaded copy needs Privacy & Security > Open Anyway on first launch.

# Releasing Monaged

Releases are built and published by GitHub Actions on Windows (`.github/workflows/release.yml`). Each one has three files:

- `Monaged-Setup-<version>.exe`: the installer (the app itself, named so it installs). Installed copies update from this file.
- `monaged-<version>-windows-x64.zip`: the same app to run without installing, with the readme, license and changelog.
- `SHA256SUMS.txt`: checksums. The updater refuses a download that doesn't match.

## Steps

1. Set the version in `Cargo.toml` and run `cargo check` so `Cargo.lock` follows.
2. Move the "Unreleased" notes in `CHANGELOG.md` under `## [<version>] - <date>`. The release notes are that section plus install instructions (`scripts/release-notes.ps1`).
3. Commit, then publish in one of two ways:
   - push a tag: `git tag v<version>` and `git push origin v<version>`; or
   - make the commit's message start with `Release v<version>` and push it to `main` (or a `claude/` branch). The workflow creates the tag on that commit.

   Don't push changes to `.github/workflows/` while a release is on its way out. Publishing creates the tag `v<version>` at the release commit, and GitHub refuses (`HTTP 403: Resource not accessible by integration`) to let the Actions token create a tag at a commit whose workflow files differ from `main`'s. If that happens, make a new `Release v<version>` commit on top of `main` and push it: nothing was published, so the version is still free.

   Push the release commit to one branch only. Pushed to `main` and a `claude/` branch, it starts two runs that both publish, and the second rebuilds and replaces the files the first one uploaded. To bring the other branch along afterwards, push it with a later commit on top: the workflow only looks at the newest commit of a push.
4. The workflow checks that the tag matches `Cargo.toml`, runs the tests, builds the release and packages it on Windows, then a small Linux job checks the checksums and creates the GitHub release (a version with a `-`, like `0.4.0-beta.1`, becomes a pre-release). Publishing happens on Linux because the release call from the Windows job was refused once, while the same call from Linux works.

Running the workflow by hand (Actions → Release → Run workflow) builds the same files as a downloadable artifact without publishing anything.

## Building locally

`scripts/release.ps1` builds and packages on your PC into `dist\`. With `-Publish` it also creates the GitHub release through `gh`.

## Signing

The installer isn't code-signed yet, so SmartScreen warns about it. With a code-signing certificate, sign `target\release\monaged.exe` with `signtool sign /fd SHA256 /tr <timestamp server> /td SHA256` before `scripts/package.ps1` runs; the checksums are computed after packaging.

## When the repository goes public

- Updates then work for everyone without the GitHub CLI.
- To publish the website in `site/`: Settings → Pages → Source: **GitHub Actions**, then run the **Website** workflow (Actions → Website → Run workflow). It deploys `site/` as is.
- Set the repository's social preview (Settings → General → Social preview) to `assets/brand/social-preview.png`.

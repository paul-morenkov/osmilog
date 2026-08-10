---
name: release
description: Cut an osmilog release — bump the version, build, commit, tag, and push. Use when the user wants to release, publish, cut a version, or tag a new osmilog version.
---

# Cutting a release

osmilog uses manual, deliberate version bumps — no automation script, just a short command
sequence. Do the version bump and commit; only tag/push when the user confirms.

1. Bump `version` in `Cargo.toml`.
2. `cargo build` — refreshes `Cargo.lock`'s matching entry.
3. Commit and push to `main`:
   ```
   git commit -am "chore: bump version to X.Y.Z"
   git push
   ```
4. Tag the commit and push the tag:
   ```
   git tag vX.Y.Z
   git push --tags
   ```

## What the push does

- Pushing the **tag** triggers `.github/workflows/release.yaml`, which checks that the tag
  matches `Cargo.toml`'s version (**failing the run if they've drifted**) and publishes a GitHub
  Release with auto-generated notes.
- GitHub Pages already rebuilds and redeploys on every push to `main` via
  `.github/workflows/build-wasm.yaml`; every build embeds the crate version plus the exact commit
  SHA it was built from (shown in the app's menu bar, e.g. `v0.2.0 (a450d44)`). So the live site is
  always current and identifiable — tagging a release doesn't trigger a redeploy, it just stamps
  an already-live commit with a human-meaningful version and release notes.

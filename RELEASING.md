# Releasing

osmilog uses manual, deliberate version bumps: there's no bump-automation script, just
a short sequence of commands.

1. Bump `version` in `Cargo.toml`.
2. `cargo build` (refreshes `Cargo.lock`'s matching entry).
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

Pushing the tag triggers `.github/workflows/release.yaml`, which checks that the tag
matches `Cargo.toml`'s version (failing the run if they've drifted) and publishes a
GitHub Release with auto-generated notes.

GitHub Pages already rebuilds and redeploys on every push to `main` via
`.github/workflows/build-wasm.yaml`, and every build embeds the crate version plus the
exact commit SHA it was built from (shown in the app's menu bar, e.g. `v0.2.0 (a450d44)`).
So the live site is always both current and identifiable - tagging a release doesn't
trigger a redeploy, it just stamps an already-live commit with a human-meaningful version
and release notes.

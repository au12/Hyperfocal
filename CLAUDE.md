# Hyperfocal: notes for Claude

## Releasing ("release vX.Y.Z", "push as vX.Y.Z")

Cloud sessions can't push tags: the GitHub proxy only accepts branch pushes (and the API
must not be used to make one). A release is a version bump pushed to `main` instead:

1. Write the notes in `docs/releases/vX.Y.Z.md`, for users: what's new, changed shortcuts,
   install lines (follow the previous release's file).
2. `scripts/release.sh X.Y.Z` writes the version into `server/Cargo.toml`, `server/Cargo.lock`,
   `web/package.json` and `web/package-lock.json` and commits `Release vX.Y.Z`.
3. Push to `main`. `.github/workflows/release.yml` tags the commit, publishes the GitHub
   release from the notes, and starts Docker (every test, the 300,000-photo test, then the
   versioned images) and Fuzz on the tag.
4. Check: `gh run list -R au12/Hyperfocal` and
   `gh api repos/au12/Hyperfocal/releases/tags/vX.Y.Z` (REST works; GraphQL doesn't).

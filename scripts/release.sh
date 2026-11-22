#!/usr/bin/env bash
# Prepares a release: writes the new version everywhere it is kept and commits it.
#
#   scripts/release.sh 0.1.2
#   git push origin HEAD:main
#
# The push is the release: .github/workflows/release.yml tags the commit v0.1.2,
# publishes the GitHub release and starts the release runs (all tests, then the
# versioned images). Its notes are docs/releases/v0.1.2.md: write them first, or edit
# the draft this script makes from the commit list and amend the commit before pushing.
set -euo pipefail

v="${1:-}"
if ! [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: $0 X.Y.Z" >&2
  exit 2
fi
cd "$(git rev-parse --show-toplevel)"

if [[ -n "$(git status --porcelain --untracked-files=no -- . ':!docs/releases')" ]]; then
  echo "Commit or stash your changes first (release notes in docs/releases may be pending)." >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v$v" > /dev/null || git ls-remote --exit-code --tags origin "refs/tags/v$v" > /dev/null 2>&1; then
  echo "v$v is already released." >&2
  exit 1
fi

old=$(sed -n 's/^version = "\(.*\)"$/\1/p' server/Cargo.toml | head -n1)

# The first match only, so dependencies' versions stay as they are. perl, for macOS too.
perl -0pi -e "s/^version = \"[^\"]*\"/version = \"$v\"/m" server/Cargo.toml
perl -0pi -e "s/(\nname = \"hyperfocal\"\nversion = )\"[^\"]*\"/\$1\"$v\"/" server/Cargo.lock
perl -0pi -e "s/\"version\": \"[^\"]*\"/\"version\": \"$v\"/" web/package.json
perl -0pi -e "s/\"version\": \"[^\"]*\"/\"version\": \"$v\"/" web/package-lock.json
perl -0pi -e "s/(\"packages\": \{\s*\"\": \{\s*\"name\": \"[^\"]*\",\s*\"version\": )\"[^\"]*\"/\$1\"$v\"/" web/package-lock.json

for f in server/Cargo.toml server/Cargo.lock web/package.json web/package-lock.json; do
  grep -q "\"$v\"" "$f" || { echo "Could not set the version in $f" >&2; exit 1; }
done

notes="docs/releases/v$v.md"
if [[ ! -f "$notes" ]]; then
  prev=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2> /dev/null || true)
  mkdir -p docs/releases
  {
    echo "## Changes"
    echo
    git log --no-merges --format='- %s' ${prev:+"$prev"..}HEAD
    echo
    echo "## Install"
    echo
    echo "The image is \`ghcr.io/au12/hyperfocal:$v\` (linux/amd64). \`:${v%.*}\` follows the ${v%.*}.x releases, and \`:latest\` follows \`main\`."
  } > "$notes"
  echo "Wrote a draft of the notes from the commits: $notes"
fi

git add server/Cargo.toml server/Cargo.lock web/package.json web/package-lock.json "$notes"
git commit -q -m "Release v$v"
echo "Version $old -> $v, committed. Push it to main to release: git push origin HEAD:main"

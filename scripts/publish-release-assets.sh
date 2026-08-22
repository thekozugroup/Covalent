#!/bin/sh
# Publish verified artifacts to the GitHub Release for an existing tag.
#
#   scripts/publish-release-assets.sh vX.Y.Z FILE...
#
# Every platform workflow calls this after its own verification has passed, so
# the release page is assembled incrementally and idempotently. The release is
# only ever created for a tag that already exists (`--verify-tag`), and assets
# are uploaded with `--clobber` so a re-run of a workflow replaces rather than
# duplicates its own artifacts.
#
# The release is created as a DRAFT and nothing here ever publishes it. Assembly
# is incremental by design, but visibility should not be: no lane knows whether
# it is the last one to finish, so any lane that published on its own way out
# would expose a half-assembled release page. A draft is invisible to anyone
# without push access, so the page only becomes public when a human has looked
# at it, confirmed every expected asset is present with the right size and
# checksum, and run:
#
#   gh release edit vX.Y.Z --draft=false
#
# Two consequences worth knowing before relying on this:
#   - A draft is not bound to its tag in the web UI; it lives at an
#     `untagged-<hash>` URL until it is published.
#   - `gh release view` with no tag argument means "latest published release"
#     and does NOT see drafts. android-release.yml resolves the prior signed
#     package that way, so a release left in draft reads as absent and that
#     upgrade gate will correctly refuse to run without `first_release`.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

: "${GH_TOKEN:?GH_TOKEN is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

version=${1:?usage: publish-release-assets.sh vX.Y.Z FILE...}
shift
[ "$#" -gt 0 ] || {
  echo "at least one asset is required" >&2
  exit 1
}

case "$version" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "release version must be an explicit v-prefixed semantic version: $version" >&2
    exit 1
    ;;
esac

for asset in "$@"; do
  [ -f "$asset" ] || {
    echo "release asset is missing: $asset" >&2
    exit 1
  }
done

ensure_release() {
  attempt=1
  while [ "$attempt" -le 5 ]; do
    if gh release view "$version" --repo "$GITHUB_REPOSITORY" >/dev/null 2>&1; then
      return 0
    fi
    set -- --repo "$GITHUB_REPOSITORY" --verify-tag --draft --title "Covalent $version"
    notes="$repo_root/docs/release/notes/$version.md"
    if [ -f "$notes" ]; then
      set -- "$@" --notes-file "$notes"
    else
      set -- "$@" --generate-notes
    fi
    case "$version" in
      *-*) set -- "$@" --prerelease ;;
    esac
    if gh release create "$version" "$@"; then
      return 0
    fi
    # Another platform workflow most likely created it in the same moment.
    echo "release creation attempt $attempt did not succeed; re-checking" >&2
    attempt=$((attempt + 1))
    sleep 5
  done
  echo "could not create or observe the release for $version" >&2
  exit 1
}

ensure_release

for asset in "$@"; do
  gh release upload "$version" "$asset" --repo "$GITHUB_REPOSITORY" --clobber
  echo "uploaded $(basename "$asset")"
done

echo "release $version now carries:"
gh release view "$version" --repo "$GITHUB_REPOSITORY" --json assets --jq '.assets[] | "  \(.name)  \(.size) bytes"'

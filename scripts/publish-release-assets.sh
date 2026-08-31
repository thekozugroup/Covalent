#!/bin/sh
# Publish verified artifacts to the GitHub Release for an existing tag.
#
#   scripts/publish-release-assets.sh vX.Y.Z FILE...
#
# Every platform workflow calls this after its own verification has passed, so
# the release page is assembled incrementally and idempotently. The release is
# only ever created for a tag that already exists (`--verify-tag`). Authenticated
# release-list discovery and numeric release/asset IDs make draft lookup and
# idempotent replacement independent of GitHub's published tag endpoint.
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

if ! printf '%s\n' "$version" \
  | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$'; then
  echo "release version must be an explicit v-prefixed semantic version: $version" >&2
  exit 1
fi

for asset in "$@"; do
  [ -f "$asset" ] || {
    echo "release asset is missing: $asset" >&2
    exit 1
  }
done

release_record() {
  # Draft releases are not reliably addressable through the tag endpoint. List
  # every release visible to the authenticated publisher and select the exact
  # tag instead. `--slurp` makes pagination one valid JSON value before jq runs.
  gh api --paginate --slurp \
    "repos/${GITHUB_REPOSITORY}/releases?per_page=100" \
    --jq ".[][] | select(.tag_name == \"${version}\") | [.id, .draft, .upload_url] | @tsv"
}

require_one_release_id() {
  records=$1
  first=$(printf '%s\n' "$records" | sed -n '1p')
  second=$(printf '%s\n' "$records" | sed -n '2p')
  if [ -n "$second" ]; then
    echo "multiple releases exist for $version; refusing an ambiguous upload" >&2
    exit 1
  fi
  release_id=$(printf '%s\n' "$first" | awk -F '\t' '{print $1}')
  release_draft=$(printf '%s\n' "$first" | awk -F '\t' '{print $2}')
  release_upload_template=$(printf '%s\n' "$first" | awk -F '\t' '{print $3}')
  case "$release_id" in
    ''|*[!0-9]*)
      echo "release $version has no valid numeric release ID" >&2
      exit 1
      ;;
  esac
  case "$release_draft" in
    true|false) ;;
    *)
      echo "release $version has an invalid draft state" >&2
      exit 1
      ;;
  esac
  expected_upload_template="https://uploads.github.com/repos/${GITHUB_REPOSITORY}/releases/${release_id}/assets{?name,label}"
  if [ "$release_upload_template" != "$expected_upload_template" ]; then
    echo "release $version has an unexpected asset upload URL" >&2
    exit 1
  fi
  printf '%s\t%s\n' "$release_id" "$release_upload_template"
}

ensure_release() {
  attempt=1
  while [ "$attempt" -le 5 ]; do
    records=$(release_record)
    if [ -n "$records" ]; then
      require_one_release_id "$records"
      return
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
    if gh release create "$version" "$@" >/dev/null; then
      # Resolve the numeric ID from the authenticated list on the next pass;
      # neither later discovery nor upload depends on a draft tag endpoint.
      attempt=$((attempt + 1))
      sleep 1
      continue
    fi
    # Another platform workflow most likely created it in the same moment.
    echo "release creation attempt $attempt did not succeed; re-checking" >&2
    attempt=$((attempt + 1))
    sleep 5
  done
  echo "could not create or observe the release for $version" >&2
  exit 1
}

release_identity=$(ensure_release)
release_id=$(printf '%s\n' "$release_identity" | awk -F '\t' '{print $1}')
release_upload_template=$(printf '%s\n' "$release_identity" | awk -F '\t' '{print $2}')
release_upload_url=$(printf '%s\n' "$release_upload_template" | sed 's/{?name,label}$//')

upload_asset() {
  asset=$1
  asset_name=$(basename "$asset")
  case "$asset_name" in
    ''|*[!A-Za-z0-9._-]*)
      echo "release asset name contains unsupported characters: $asset_name" >&2
      exit 1
      ;;
  esac

  # Clobber through the numeric release ID as well. A tag lookup can miss a
  # draft, which is exactly when all automated lanes upload their artifacts.
  existing_ids=$(gh api --paginate --slurp \
    "repos/${GITHUB_REPOSITORY}/releases/${release_id}/assets?per_page=100" \
    --jq ".[][] | select(.name == \"${asset_name}\") | .id")
  for existing_id in $existing_ids; do
    case "$existing_id" in
      ''|*[!0-9]*)
        echo "release asset $asset_name has an invalid asset ID" >&2
        exit 1
        ;;
    esac
    gh api --method DELETE \
      "repos/${GITHUB_REPOSITORY}/releases/assets/${existing_id}" --silent
  done

  # GitHub returns a dedicated uploads.github.com endpoint. Posting the bytes to
  # api.github.com is not the release-asset API even though the path is equal.
  curl --fail --silent --show-error --location --request POST \
    --header "Accept: application/vnd.github+json" \
    --header "Authorization: Bearer ${GH_TOKEN}" \
    --header "X-GitHub-Api-Version: 2022-11-28" \
    --header "Content-Type: application/octet-stream" \
    --data-binary "@${asset}" \
    "${release_upload_url}?name=${asset_name}" >/dev/null
}

for asset in "$@"; do
  upload_asset "$asset"
  echo "uploaded $(basename "$asset")"
done

echo "release $version now carries:"
gh api --paginate "repos/${GITHUB_REPOSITORY}/releases/${release_id}/assets?per_page=100" \
  --jq '.[] | "  \(.name)  \(.size) bytes"'

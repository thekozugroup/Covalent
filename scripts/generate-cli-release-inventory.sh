#!/bin/sh
# Emit a machine-readable dependency and license inventory for a CLI archive.
set -eu

usage() {
  echo "usage: $0 --binary PATH --platform PLATFORM --version vX.Y.Z --output PATH" >&2
  exit 2
}

binary=""
platform=""
version=""
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --binary) binary=${2:-}; shift 2 ;;
    --platform) platform=${2:-}; shift 2 ;;
    --version) version=${2:-}; shift 2 ;;
    --output) output=${2:-}; shift 2 ;;
    *) usage ;;
  esac
done

[ -n "$binary" ] && [ -n "$platform" ] && [ -n "$version" ] && [ -n "$output" ] || usage
[ -f "$binary" ] || { echo "CLI binary is missing: $binary" >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "cargo is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
metadata=$(mktemp "${TMPDIR:-/tmp}/covalent-cli-metadata.XXXXXX")
trap 'rm -f "$metadata"' EXIT INT TERM
(cd "$repo_root" && cargo metadata --locked --format-version 1) > "$metadata"

# Missing licensing information is a release failure, not a documentation gap.
jq -e '[.packages[] | select(.source != null and (.license == null or .license == ""))] | length == 0' "$metadata" >/dev/null || {
  echo "a third-party CLI dependency has no declared license" >&2
  exit 1
}

# Mirror the repository dependency-review policy. LGPL is not GPL-3.0-only and
# is therefore intentionally not matched by this expression.
jq -e '[.packages[] | select(.source != null) | select(.license | test("(^|[^A-Z])AGPL-3\\.0|(^|[^L])GPL-3\\.0"))] | length == 0' "$metadata" >/dev/null || {
  echo "CLI dependency inventory contains a disallowed AGPL-3.0-only or GPL-3.0-only license" >&2
  exit 1
}

binary_bytes=$(wc -c < "$binary" | tr -d '[:space:]')
commit=$(cd "$repo_root" && git rev-parse HEAD)
mkdir -p "$(dirname "$output")"
jq \
  --arg artifact "$(basename "$binary")" \
  --arg platform "$platform" \
  --arg version "$version" \
  --arg commit "$commit" \
  --argjson binary_bytes "$binary_bytes" \
  '{
    schema: "https://covalent.life/schemas/cli-release-inventory/v1",
    artifact: $artifact,
    platform: $platform,
    version: $version,
    commit: $commit,
    binary_bytes: $binary_bytes,
    license_policy: { denied: ["AGPL-3.0-only", "GPL-3.0-only"] },
    packages: [
      .packages[]
      | select(.source != null)
      | {name, version, license, source, checksum: .checksum}
    ]
  }' "$metadata" > "$output"

jq -e '.packages | length > 0' "$output" >/dev/null
echo "wrote CLI dependency inventory: $output"

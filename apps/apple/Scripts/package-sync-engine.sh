#!/bin/sh
# Build and materialize the pinned macOS arm64 sync engine in one Xcode product.
set -eu

test "$#" -eq 2 || { echo "usage: $0 MACOS-DIRECTORY RESOURCE-DIRECTORY" >&2; exit 64; }
test "$(uname -s)" = Darwin && test "$(uname -m)" = arm64 || {
  echo "sync-engine packaging requires Apple Silicon macOS" >&2
  exit 69
}

macos_directory=$1
resource_directory=$2
script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
apple_directory=$(CDPATH='' cd -- "$script_directory/.." && pwd)
repo_root=$(CDPATH='' cd -- "$apple_directory/../.." && pwd)
builder="$repo_root/scripts/build-macos-sync-engine.sh"
guardian_source="$repo_root/packaging/sync-engine/engine-guardian.c"
provenance_source="$apple_directory/SyncEngine/PROVENANCE.txt"
source_dir=${COVALENT_SYNCTHING_SOURCE_DIR:-${SYNCTHING_SOURCE_DIR:-}}
test -n "$source_dir" || { echo "set SYNCTHING_SOURCE_DIR to the pinned checkout" >&2; exit 66; }

for path in "$macos_directory" "$resource_directory"; do
  if test -L "$path" || { test -e "$path" && test ! -d "$path"; }; then
    echo "sync-engine destination is unsafe" >&2; exit 1
  fi
done
mkdir -p "$macos_directory" "$resource_directory"
for output in \
  "$macos_directory/covalent-engine-guardian" \
  "$macos_directory/covalent-syncthing" \
  "$resource_directory/manifest.json" \
  "$resource_directory/source-build.json" \
  "$resource_directory/notices-index.txt" \
  "$resource_directory/Syncthing-LICENSE.txt" \
  "$resource_directory/Syncthing-AUTHORS.txt" \
  "$resource_directory/PROVENANCE.txt"
do
  if test -L "$output" || { test -e "$output" && test ! -f "$output"; }; then
    echo "sync-engine output is unsafe" >&2; exit 1
  fi
done
if test -L "$resource_directory/notices" ||
  { test -e "$resource_directory/notices" && test ! -d "$resource_directory/notices"; }; then
  echo "sync-engine notice output is unsafe" >&2; exit 1
fi

working_parent=${DERIVED_FILE_DIR:-${TMPDIR:-/tmp}}
if test -L "$working_parent" || { test -e "$working_parent" && test ! -d "$working_parent"; }; then
  echo "sync-engine working directory is unsafe" >&2; exit 1
fi
mkdir -p "$working_parent"
working_root=$(mktemp -d "$working_parent/covalent-sync-engine.XXXXXX")
cleanup() {
  chmod -R u+w "$working_root" 2>/dev/null || true
  rm -rf "$working_root"
}
trap cleanup EXIT HUP INT TERM
build_output="$working_root/build"
"$builder" "$source_dir" "$build_output"

# Incremental Xcode builds may reuse the product directory. Replace only these
# already-validated package-owned paths so stale notice members cannot survive.
rm -rf "$resource_directory/notices"
rm -f "$resource_directory/manifest.json" "$resource_directory/source-build.json" \
  "$resource_directory/notices-index.txt" "$resource_directory/Syncthing-LICENSE.txt" \
  "$resource_directory/Syncthing-AUTHORS.txt" "$resource_directory/PROVENANCE.txt"

worker_source="$build_output/covalent-syncthing"
guardian="$macos_directory/covalent-engine-guardian"
worker="$macos_directory/covalent-syncthing"
xcrun clang -std=c11 -Os -Wall -Wextra -Werror -arch arm64 \
  -mmacosx-version-min=15.0 "$guardian_source" -o "$guardian"
ditto "$worker_source" "$worker"
test "$(shasum -a 256 "$worker" | awk '{print $1}')" = \
  4df1dea892fac3c9d1c0e822827c9a4c57711dcddaaf56a61edaab41d5337bbb || {
  echo "copied sync-engine executable differs from the reviewed source build" >&2; exit 1
}
chmod 755 "$guardian" "$worker"

for name in Syncthing-LICENSE.txt Syncthing-AUTHORS.txt source-build.json; do
  ditto "$build_output/$name" "$resource_directory/$name"
done
ditto "$provenance_source" "$resource_directory/PROVENANCE.txt"
ditto "$build_output/notices" "$resource_directory/notices"
combined="$resource_directory/notices/THIRD-PARTY-NOTICES.txt"
notice_manifest="$resource_directory/notices/manifest.json"
combined_sha=$(shasum -a 256 "$combined" | awk '{print $1}')
combined_bytes=$(wc -c < "$combined" | tr -d '[:space:]')
notice_manifest_sha=$(shasum -a 256 "$notice_manifest" | awk '{print $1}')
notice_manifest_bytes=$(wc -c < "$notice_manifest" | tr -d '[:space:]')
cat > "$resource_directory/notices-index.txt" <<EOF
1
combined $combined_sha $combined_bytes notices/THIRD-PARTY-NOTICES.txt
manifest $notice_manifest_sha $notice_manifest_bytes notices/manifest.json
EOF
find "$resource_directory/notices" -type d -exec chmod 755 {} +
find "$resource_directory/notices" -type f -exec chmod 644 {} +
chmod 644 "$resource_directory"/*.txt "$resource_directory"/*.json

for binary in "$guardian" "$worker"; do
  test "$(xcrun lipo -archs "$binary")" = arm64 || {
    echo "sync-engine executable is not arm64-only" >&2; exit 1
  }
done
worker_dependencies=$(otool -L "$worker" | tail -n +2 | sed 's/^[[:space:]]*//' | awk '{print $1}' | LC_ALL=C sort)
expected_worker_dependencies='/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation
/System/Library/Frameworks/CoreServices.framework/Versions/A/CoreServices
/System/Library/Frameworks/Security.framework/Versions/A/Security
/usr/lib/libSystem.B.dylib
/usr/lib/libresolv.9.dylib'
test "$worker_dependencies" = "$expected_worker_dependencies" || {
  echo "sync-engine executable has an unexpected dynamic dependency" >&2; exit 1
}
test "$(otool -L "$guardian" | tail -n +2 | sed 's/^[[:space:]]*//' | awk '{print $1}')" = \
  /usr/lib/libSystem.B.dylib || {
  echo "engine guardian has an unexpected dynamic dependency" >&2; exit 1
}
vtool -show-build "$worker" | grep -Eq 'minos[[:space:]]+15\.0' || {
  echo "sync-engine minimum macOS deployment target differs" >&2; exit 1
}

"$script_directory/sign-sync-engine-bundle.sh" \
  "$macos_directory" "$resource_directory" - adhoc

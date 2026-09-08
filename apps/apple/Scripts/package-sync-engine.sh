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
python3 - "$resource_directory/notices" "$notice_manifest" <<'PY'
import hashlib
import json
import os
import pathlib
import re
import stat
import sys

root = pathlib.Path(sys.argv[1])
manifest = json.loads(pathlib.Path(sys.argv[2]).read_bytes())
records = manifest.get("correspondingSources")
if not isinstance(records, list) or not records or len(records) > 2_048:
    raise SystemExit("sync-engine corresponding-source manifest is incomplete")
seen = set()
total = 0
for record in records:
    archive = record.get("archive") if isinstance(record, dict) else None
    if not isinstance(archive, dict):
        raise SystemExit("sync-engine corresponding-source record is malformed")
    relative = archive.get("bundlePath")
    expected_bytes = archive.get("bytes")
    expected_sha = archive.get("sha256")
    if (
        not isinstance(relative, str)
        or re.fullmatch(r"sources/[0-9]{4}-source\.tar\.gz", relative) is None
        or relative in seen
        or not isinstance(expected_bytes, int)
        or isinstance(expected_bytes, bool)
        or not 1 <= expected_bytes <= 64 * 1024 * 1024
        or not isinstance(expected_sha, str)
        or re.fullmatch(r"[0-9a-f]{64}", expected_sha) is None
        or not isinstance(record.get("externalSourceUrl"), str)
        or not record["externalSourceUrl"].startswith("https://")
    ):
        raise SystemExit("sync-engine corresponding-source record is malformed")
    seen.add(relative)
    candidate = root.joinpath(*pathlib.PurePosixPath(relative).parts)
    current = root
    for part in pathlib.PurePosixPath(relative).parts:
        current = current / part
        mode = current.lstat().st_mode
        if stat.S_ISLNK(mode):
            raise SystemExit("sync-engine corresponding-source path contains a symlink")
    descriptor = os.open(
        candidate,
        os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
    )
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size != expected_bytes:
            raise SystemExit("sync-engine corresponding-source archive differs")
        digest = hashlib.sha256()
        retained = 0
        while retained <= expected_bytes:
            chunk = os.read(descriptor, min(1024 * 1024, expected_bytes + 1 - retained))
            if not chunk:
                break
            retained += len(chunk)
            digest.update(chunk)
        after = os.fstat(descriptor)
        identity = lambda value: (
            value.st_dev, value.st_ino, value.st_size,
            value.st_mtime_ns, value.st_ctime_ns,
        )
        if (
            retained != expected_bytes
            or digest.hexdigest() != expected_sha
            or identity(before) != identity(after)
        ):
            raise SystemExit("sync-engine corresponding-source archive differs")
    finally:
        os.close(descriptor)
    total += expected_bytes
if total > 64 * 1024 * 1024:
    raise SystemExit("sync-engine corresponding-source archives exceed their bound")
PY
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

#!/bin/sh
# Materialize the pinned macOS arm64 sync engine inside one Xcode build product.
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 MACOS-DIRECTORY RESOURCE-DIRECTORY" >&2
  exit 64
fi
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo "sync-engine packaging requires an Apple Silicon Mac" >&2
  exit 69
fi

macos_directory=$1
resource_directory=$2
script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
apple_directory=$(CDPATH='' cd -- "$script_directory/.." && pwd)
repo_root=$(CDPATH='' cd -- "$apple_directory/../.." && pwd)
source_directory="$repo_root/packaging/sync-engine"
guardian_source="$source_directory/engine-guardian.c"
provenance_source="$apple_directory/SyncEngine/PROVENANCE.txt"
archive_name=syncthing-macos-arm64-v2.1.3.zip
archive_url="https://github.com/syncthing/syncthing/releases/download/v2.1.3/$archive_name"
archive_sha=e0f0d8df05bf0118c48c6515214a96bf3a3f11dbd115f56c3c0b52251b3f71aa
worker_sha=6743a0efbf9d39784c7fe2e925505cd0a839458346444aa28620341e47ba12b8
guardian_sha=c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
license_sha=9221c2f936159b8446d329249fb4c0f25be510f447383a0f13336ac7985668a3
authors_sha=2c197afd6113ec13ae134a20ce8f101d9812b078c3ef32c42c3bb4cdaf2b96a2

for path in "$macos_directory" "$resource_directory"; do
  if [ -L "$path" ] || { [ -e "$path" ] && [ ! -d "$path" ]; }; then
    echo "sync-engine destination is unsafe" >&2
    exit 1
  fi
done
mkdir -p "$macos_directory" "$resource_directory"
for output in \
  "$macos_directory/covalent-engine-guardian" \
  "$macos_directory/covalent-syncthing" \
  "$resource_directory/manifest.json" \
  "$resource_directory/Syncthing-LICENSE.txt" \
  "$resource_directory/Syncthing-AUTHORS.txt" \
  "$resource_directory/PROVENANCE.txt"
do
  if [ -L "$output" ] || { [ -e "$output" ] && [ ! -f "$output" ]; }; then
    echo "sync-engine output is unsafe" >&2
    exit 1
  fi
done

if [ -n "${COVALENT_SYNCTHING_ARCHIVE:-}" ]; then
  archive=$COVALENT_SYNCTHING_ARCHIVE
else
  cache_directory=${DERIVED_FILE_DIR:-${TMPDIR:-/tmp}/covalent-sync-engine-cache}
  if [ -L "$cache_directory" ] || { [ -e "$cache_directory" ] && [ ! -d "$cache_directory" ]; }; then
    echo "sync-engine cache is unsafe" >&2
    exit 1
  fi
  mkdir -p "$cache_directory"
  archive="$cache_directory/$archive_name"
  if [ ! -e "$archive" ]; then
    temporary_archive="$cache_directory/.$archive_name.$$.tmp"
    if [ -e "$temporary_archive" ] || [ -L "$temporary_archive" ]; then
      echo "refusing an existing temporary sync-engine archive" >&2
      exit 1
    fi
    if ! curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
      --connect-timeout 15 --max-time 300 --max-filesize 67108864 \
      --output "$temporary_archive" "$archive_url"; then
      rm -f "$temporary_archive"
      exit 1
    fi
    actual=$(shasum -a 256 "$temporary_archive" | awk '{print $1}')
    if [ "$actual" != "$archive_sha" ]; then
      rm -f "$temporary_archive"
      echo "downloaded sync-engine archive failed checksum validation" >&2
      exit 1
    fi
    mv -n "$temporary_archive" "$archive"
    if [ -e "$temporary_archive" ]; then
      echo "sync-engine cache collision" >&2
      exit 1
    fi
  fi
fi

if [ ! -f "$archive" ] || [ -L "$archive" ]; then
  echo "sync-engine archive is missing or unsafe" >&2
  exit 1
fi
actual=$(shasum -a 256 "$archive" | awk '{print $1}')
if [ "$actual" != "$archive_sha" ]; then
  echo "sync-engine archive failed checksum validation" >&2
  exit 1
fi
actual=$(shasum -a 256 "$guardian_source" | awk '{print $1}')
if [ "$actual" != "$guardian_sha" ]; then
  echo "engine guardian source failed checksum validation" >&2
  exit 1
fi

working_root=${DERIVED_FILE_DIR:-${TMPDIR:-/tmp}}
if [ -L "$working_root" ] || { [ -e "$working_root" ] && [ ! -d "$working_root" ]; }; then
  echo "sync-engine working directory is unsafe" >&2
  exit 1
fi
mkdir -p "$working_root"
extract_directory=$(mktemp -d "$working_root/covalent-sync-engine.XXXXXX")
cleanup() {
  python3 - "$extract_directory" <<'PY'
import shutil
import sys
shutil.rmtree(sys.argv[1], ignore_errors=True)
PY
}
trap cleanup EXIT HUP INT TERM

/usr/bin/unzip -q "$archive" -d "$extract_directory"
release_directory="$extract_directory/syncthing-macos-arm64-v2.1.3"
worker_source="$release_directory/syncthing"
license_source="$release_directory/LICENSE.txt"
authors_source="$release_directory/AUTHORS.txt"
verify_release_member() {
  source_path=$1
  expected=$2
  if [ ! -f "$source_path" ] || [ -L "$source_path" ]; then
    echo "sync-engine release is missing a required regular file" >&2
    exit 1
  fi
  actual=$(shasum -a 256 "$source_path" | awk '{print $1}')
  if [ "$actual" != "$expected" ]; then
    echo "sync-engine release member failed checksum validation" >&2
    exit 1
  fi
}
verify_release_member "$worker_source" "$worker_sha"
verify_release_member "$license_source" "$license_sha"
verify_release_member "$authors_source" "$authors_sha"

guardian="$macos_directory/covalent-engine-guardian"
worker="$macos_directory/covalent-syncthing"
xcrun clang -std=c11 -Os -Wall -Wextra -Werror -arch arm64 \
  -mmacosx-version-min=15.0 "$guardian_source" -o "$guardian"
ditto "$worker_source" "$worker"
chmod 755 "$guardian" "$worker"
if [ "$(shasum -a 256 "$worker" | awk '{print $1}')" != "$worker_sha" ]; then
  echo "copied sync-engine executable changed before signing" >&2
  exit 1
fi
ditto "$license_source" "$resource_directory/Syncthing-LICENSE.txt"
ditto "$authors_source" "$resource_directory/Syncthing-AUTHORS.txt"
ditto "$provenance_source" "$resource_directory/PROVENANCE.txt"
chmod 644 "$resource_directory/Syncthing-LICENSE.txt" \
  "$resource_directory/Syncthing-AUTHORS.txt" "$resource_directory/PROVENANCE.txt"

for binary in "$guardian" "$worker"; do
  architectures=$(xcrun lipo -archs "$binary")
  if [ "$architectures" != arm64 ]; then
    echo "sync-engine executable is not arm64-only" >&2
    exit 1
  fi
done

worker_dependencies=$(otool -L "$worker" | tail -n +2 | sed 's/^[[:space:]]*//' | awk '{print $1}' | LC_ALL=C sort)
expected_worker_dependencies='/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation
/System/Library/Frameworks/CoreServices.framework/Versions/A/CoreServices
/System/Library/Frameworks/Security.framework/Versions/A/Security
/usr/lib/libSystem.B.dylib
/usr/lib/libresolv.9.dylib'
if [ "$worker_dependencies" != "$expected_worker_dependencies" ]; then
  echo "sync-engine executable has an unexpected dynamic dependency" >&2
  exit 1
fi
guardian_dependencies=$(otool -L "$guardian" | tail -n +2 | sed 's/^[[:space:]]*//' | awk '{print $1}')
if [ "$guardian_dependencies" != /usr/lib/libSystem.B.dylib ]; then
  echo "engine guardian has an unexpected dynamic dependency" >&2
  exit 1
fi

"$script_directory/sign-sync-engine-bundle.sh" \
  "$macos_directory" "$resource_directory" - adhoc

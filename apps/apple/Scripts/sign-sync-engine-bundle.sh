#!/bin/sh
# Sign the two bundled engine executables, then record their final byte hashes.
# The manifest must be regenerated after every re-sign because Mach-O signing
# changes the executable bytes.
set -eu

if [ "$#" -ne 4 ]; then
  echo "usage: $0 MACOS-DIRECTORY RESOURCE-DIRECTORY IDENTITY adhoc|timestamp" >&2
  exit 64
fi

macos_directory=$1
resource_directory=$2
identity=$3
timestamp_mode=$4

case "$timestamp_mode" in
  adhoc) timestamp_argument=--timestamp=none ;;
  timestamp) timestamp_argument=--timestamp ;;
  *) echo "invalid sync-engine timestamp mode" >&2; exit 64 ;;
esac

script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
apple_directory=$(CDPATH='' cd -- "$script_directory/.." && pwd)
entitlements="$apple_directory/Config/CovalentNode.entitlements"
guardian="$macos_directory/covalent-engine-guardian"
worker="$macos_directory/covalent-syncthing"
manifest="$resource_directory/manifest.json"
license="$resource_directory/Syncthing-LICENSE.txt"
authors="$resource_directory/Syncthing-AUTHORS.txt"
provenance="$resource_directory/PROVENANCE.txt"

if [ -L "$manifest" ] || { [ -e "$manifest" ] && [ ! -f "$manifest" ]; }; then
  echo "sync-engine manifest is unsafe" >&2
  exit 1
fi

for path in "$macos_directory" "$resource_directory"; do
  if [ ! -d "$path" ] || [ -L "$path" ]; then
    echo "sync-engine bundle directory is missing or unsafe" >&2
    exit 1
  fi
done
for binary in "$guardian" "$worker"; do
  if [ ! -f "$binary" ] || [ -L "$binary" ] || [ ! -x "$binary" ]; then
    echo "sync-engine executable is missing or unsafe" >&2
    exit 1
  fi
done
verify_notice() {
  path=$1
  expected=$2
  if [ ! -f "$path" ] || [ -L "$path" ] ||
    [ "$(shasum -a 256 "$path" | awk '{print $1}')" != "$expected" ]; then
    echo "sync-engine notice is missing or changed" >&2
    exit 1
  fi
}
verify_notice "$license" 9221c2f936159b8446d329249fb4c0f25be510f447383a0f13336ac7985668a3
verify_notice "$authors" 2c197afd6113ec13ae134a20ce8f101d9812b078c3ef32c42c3bb4cdaf2b96a2
verify_notice "$provenance" dd8b63bb770c8fc9e6d0c151e575def944e1502f81a651e7ec72588951ebacd9

sign_one() {
  identifier=$1
  binary=$2
  if [ -n "${COVALENT_SIGNING_KEYCHAIN:-}" ]; then
    codesign --force --sign "$identity" --identifier "$identifier" \
      --options runtime "$timestamp_argument" \
      --keychain "$COVALENT_SIGNING_KEYCHAIN" \
      --entitlements "$entitlements" "$binary"
  elif [ "$identity" = "-" ]; then
    codesign --force --sign - --identifier "$identifier" \
      --options runtime "$timestamp_argument" \
      --entitlements "$entitlements" "$binary"
  else
    codesign --force --sign "$identity" --identifier "$identifier" \
      --options runtime "$timestamp_argument" \
      --entitlements "$entitlements" "$binary"
  fi
}

sign_one life.michaelwong.covalent.engine-guardian "$guardian"
sign_one life.michaelwong.covalent.syncthing "$worker"

guardian_sha=$(shasum -a 256 "$guardian" | awk '{print $1}')
worker_sha=$(shasum -a 256 "$worker" | awk '{print $1}')
case "$guardian_sha$worker_sha" in
  *[!0-9a-f]*|'') echo "could not calculate sync-engine hashes" >&2; exit 1 ;;
esac

temporary_manifest="$resource_directory/.manifest.$$.tmp"
if [ -e "$temporary_manifest" ] || [ -L "$temporary_manifest" ]; then
  echo "refusing an existing temporary sync-engine manifest" >&2
  exit 1
fi
umask 022
python3 - "$temporary_manifest" "$guardian_sha" "$worker_sha" <<'PY'
import json
import os
import sys

path, guardian_sha, worker_sha = sys.argv[1:]
value = {
    "schema": 1,
    "engine": {
        "name": "Syncthing",
        "version": "v2.1.3",
        "commit": "946e2b83a1f6c6ae119427c09e0a5802940b82ff",
        "archiveSha256": "e0f0d8df05bf0118c48c6515214a96bf3a3f11dbd115f56c3c0b52251b3f71aa",
        "unsignedExecutableSha256": "6743a0efbf9d39784c7fe2e925505cd0a839458346444aa28620341e47ba12b8",
    },
    "guardian": {
        "sourceSha256": "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579",
    },
    "notices": {
        "PROVENANCE.txt": "dd8b63bb770c8fc9e6d0c151e575def944e1502f81a651e7ec72588951ebacd9",
        "Syncthing-AUTHORS.txt": "2c197afd6113ec13ae134a20ce8f101d9812b078c3ef32c42c3bb4cdaf2b96a2",
        "Syncthing-LICENSE.txt": "9221c2f936159b8446d329249fb4c0f25be510f447383a0f13336ac7985668a3",
    },
    "executables": {
        "covalent-engine-guardian": {
            "architecture": "arm64",
            "signedSha256": guardian_sha,
        },
        "covalent-syncthing": {
            "architecture": "arm64",
            "signedSha256": worker_sha,
        },
    },
}
encoded = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
with os.fdopen(fd, "wb") as stream:
    stream.write(encoded)
    stream.flush()
    os.fsync(stream.fileno())
PY
mv -f "$temporary_manifest" "$manifest"

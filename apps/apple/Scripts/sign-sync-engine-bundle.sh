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
worker="$macos_directory/covalent-rclone"
manifest="$resource_directory/manifest.json"
license="$resource_directory/rclone-LICENSE.txt"
provenance="$resource_directory/PROVENANCE.txt"
source_build="$resource_directory/source-build.json"
notice_index="$resource_directory/notices-index.txt"
target_notice_manifest="$resource_directory/notices/manifest.json"
combined_notices="$resource_directory/notices/THIRD-PARTY-NOTICES.txt"

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
verify_notice "$license" 9266eae9c6a441de0f6847f19ac8f09b280d55612b079eca03b3d49b822c1a71
verify_notice "$provenance" e22474484d6fe7a68be02b7685961ecf0c689c357dc540e782f67fe16058df2e
verify_notice "$source_build" f0217883095af8950ae68f1a95f9d3210faa7d566736c85fc410b401124f4618
verify_notice "$notice_index" 5868a5e73b0ea3be7c54229348a973eafe2f81d8db5f755db5f97cc697a4f75c
verify_notice "$target_notice_manifest" 5a6294b67083cf9e394b11702a9ec943a9efb2e965a7551999d7a78d25ba4fc8
verify_notice "$combined_notices" 3af0afba1e2feb48c93b2ee6b3204262784b19604133bb518bdcfc904722a223

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
sign_one life.michaelwong.covalent.rclone "$worker"

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
        "name": "rclone",
        "version": "v1.75.1",
        "commit": "687d264b689b8c49a67e2e52a8a5e0caa01c04ce",
        "sourceState": "upstream-unmodified",
        "goVersion": "go1.26.10",
        "unsignedExecutableSha256": "07cb75e9292c3c3a08322f4e34a6b1e6f91599b43ae917ae64f6d13bfb864922",
    },
    "guardian": {
        "sourceSha256": "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579",
    },
    "notices": {
        "PROVENANCE.txt": "e22474484d6fe7a68be02b7685961ecf0c689c357dc540e782f67fe16058df2e",
        "rclone-LICENSE.txt": "9266eae9c6a441de0f6847f19ac8f09b280d55612b079eca03b3d49b822c1a71",
        "source-build.json": "f0217883095af8950ae68f1a95f9d3210faa7d566736c85fc410b401124f4618",
        "notices-index.txt": "5868a5e73b0ea3be7c54229348a973eafe2f81d8db5f755db5f97cc697a4f75c",
        "notices/manifest.json": "5a6294b67083cf9e394b11702a9ec943a9efb2e965a7551999d7a78d25ba4fc8",
        "notices/THIRD-PARTY-NOTICES.txt": "3af0afba1e2feb48c93b2ee6b3204262784b19604133bb518bdcfc904722a223",
    },
    "executables": {
        "covalent-engine-guardian": {
            "architecture": "arm64",
            "signedSha256": guardian_sha,
        },
        "covalent-rclone": {
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

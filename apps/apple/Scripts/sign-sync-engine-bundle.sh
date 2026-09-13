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
verify_notice "$provenance" ce9886f37cd7bc7b62e755375f0d5f23d5f97f60670f6f857dac632b33f5e0ef
verify_notice "$source_build" 122b3ee828fb92c7149346ab02f306e53f6633cb907c5d05d7deac44ac307a2d
verify_notice "$notice_index" 59ab37c92bac32b1b3533ce8be192c282c9b7d25a81f03b2f63b7651faa4ab51
verify_notice "$target_notice_manifest" 9a624a53020218aea6927c2baa254daa7ce9099c83eeddcfed371001e889bd6d
verify_notice "$combined_notices" 9374aa12dc9da1416da235f6a664b6b5817081adf00b94a13d2c58d4a8ca68d4

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
        "goVersion": "go1.26.7",
        "unsignedExecutableSha256": "d606a368fe4b83b81080aa9913d9f24b382c601e995d25f63ab80a49e2c8dee9",
    },
    "guardian": {
        "sourceSha256": "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579",
    },
    "notices": {
        "PROVENANCE.txt": "ce9886f37cd7bc7b62e755375f0d5f23d5f97f60670f6f857dac632b33f5e0ef",
        "rclone-LICENSE.txt": "9266eae9c6a441de0f6847f19ac8f09b280d55612b079eca03b3d49b822c1a71",
        "source-build.json": "122b3ee828fb92c7149346ab02f306e53f6633cb907c5d05d7deac44ac307a2d",
        "notices-index.txt": "59ab37c92bac32b1b3533ce8be192c282c9b7d25a81f03b2f63b7651faa4ab51",
        "notices/manifest.json": "9a624a53020218aea6927c2baa254daa7ce9099c83eeddcfed371001e889bd6d",
        "notices/THIRD-PARTY-NOTICES.txt": "9374aa12dc9da1416da235f6a664b6b5817081adf00b94a13d2c58d4a8ca68d4",
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

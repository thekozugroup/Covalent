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
verify_notice "$license" 3f3d9e0024b1921b067d6f7f88deb4a60cbe7a78e76c64e3f1d7fc3b779b9d04
verify_notice "$authors" 5a0044d13ddf6f013bdd5c2bc419bf45d6123c356567510237e82f304d113d48
verify_notice "$provenance" d41289725ed1e7c76eda0cc6a3f44c5f7bebd07d578fe6c546bc04911e46ac5e
verify_notice "$source_build" 1b4a74cb976130a4bea7d4f059b3bdd282c09ed44507b1671e0256385ed35164
verify_notice "$notice_index" 545bed4faff39aebfe587e80cc439c15d407e3a8de721e319d9832129ac7304f
verify_notice "$target_notice_manifest" 85dca7cc49ffe72b75e5dc0ba49191384cf724816972e4514e2f075fed5f8b31
verify_notice "$combined_notices" 1428276dd703546c78b86c210b61b74c7ebe64fe5ee2def57fe4c8f2d4673d52

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
        "upstreamArchiveSha256": "dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959",
        "sourceExportSha256": "eb60efd57d1662af75ffb2f7b89abab7200362c654838486138a34bee00fed29",
        "goVersion": "go1.26.7",
        "unsignedExecutableSha256": "9545a14bcc3116123233a7b5b1c62700f74fea1160ae95e3f9138dec54e62014",
    },
    "guardian": {
        "sourceSha256": "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579",
    },
    "notices": {
        "PROVENANCE.txt": "d41289725ed1e7c76eda0cc6a3f44c5f7bebd07d578fe6c546bc04911e46ac5e",
        "Syncthing-AUTHORS.txt": "5a0044d13ddf6f013bdd5c2bc419bf45d6123c356567510237e82f304d113d48",
        "Syncthing-LICENSE.txt": "3f3d9e0024b1921b067d6f7f88deb4a60cbe7a78e76c64e3f1d7fc3b779b9d04",
        "source-build.json": "1b4a74cb976130a4bea7d4f059b3bdd282c09ed44507b1671e0256385ed35164",
        "notices-index.txt": "545bed4faff39aebfe587e80cc439c15d407e3a8de721e319d9832129ac7304f",
        "notices/manifest.json": "85dca7cc49ffe72b75e5dc0ba49191384cf724816972e4514e2f075fed5f8b31",
        "notices/THIRD-PARTY-NOTICES.txt": "1428276dd703546c78b86c210b61b74c7ebe64fe5ee2def57fe4c8f2d4673d52",
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

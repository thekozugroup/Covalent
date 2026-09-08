#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 /path/to/Covalent.app" >&2
  exit 64
fi

if [ "$(uname -s)" != "Darwin" ]; then
  echo "Apple Silicon bundle verification requires macOS." >&2
  exit 69
fi

app=$1
info_plist="$app/Contents/Info.plist"
if [ ! -d "$app" ] || [ -L "$app" ] || [ ! -f "$info_plist" ] || [ -L "$info_plist" ]; then
  echo "Covalent app bundle is missing: $app" >&2
  exit 1
fi

app_executable=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$info_plist")
app_binary="$app/Contents/MacOS/$app_executable"
helper="$app/Contents/MacOS/covalent-node"
guardian="$app/Contents/MacOS/covalent-engine-guardian"
worker="$app/Contents/MacOS/covalent-syncthing"
engine_resources="$app/Contents/Resources/CovalentSyncEngine"
engine_manifest="$engine_resources/manifest.json"
require_hardened_runtime=${COVALENT_REQUIRE_HARDENED_RUNTIME:-false}
require_developer_id=${COVALENT_REQUIRE_DEVELOPER_ID:-false}

for binary in "$app_binary" "$helper" "$guardian" "$worker"; do
  if [ ! -f "$binary" ] || [ -L "$binary" ] || [ ! -x "$binary" ]; then
    echo "required executable is missing: $binary" >&2
    exit 1
  fi

  architectures=$(xcrun lipo -archs "$binary")
  if [ "$architectures" != "arm64" ]; then
    echo "macOS executable must be arm64-only: $binary ($architectures)" >&2
    exit 1
  fi
  if xcrun lipo "$binary" -verify_arch x86_64 >/dev/null 2>&1; then
    echo "macOS executable unexpectedly contains x86_64: $binary" >&2
    exit 1
  fi

  codesign --verify --strict --verbose=2 "$binary"
  signature=$(codesign -d --verbose=4 "$binary" 2>&1)
  if [ "$require_hardened_runtime" = "true" ] &&
    ! printf '%s\n' "$signature" | grep -Eq 'flags=.*runtime'; then
    echo "hardened runtime is missing: $binary" >&2
    exit 1
  fi
  if [ "$require_developer_id" = "true" ] &&
    ! printf '%s\n' "$signature" | grep -q '^Authority=Developer ID Application:'; then
    echo "Developer ID signature is missing: $binary" >&2
    exit 1
  fi
done

for inherited_binary in "$helper" "$guardian" "$worker"; do
  entitlements=$(codesign -d --entitlements :- "$inherited_binary" 2>/dev/null)
  if ! printf '%s' "$entitlements" | python3 -c '
import plistlib
import sys

expected = {
    "com.apple.security.app-sandbox": True,
    "com.apple.security.inherit": True,
}
raise SystemExit(0 if plistlib.loads(sys.stdin.buffer.read()) == expected else 1)
'; then
    echo "bundled helper does not have the exact sandbox inheritance entitlements" >&2
    exit 1
  fi
done

for resource in "$engine_manifest" "$engine_resources/PROVENANCE.txt" \
  "$engine_resources/Syncthing-LICENSE.txt" "$engine_resources/Syncthing-AUTHORS.txt"; do
  if [ ! -f "$resource" ] || [ -L "$resource" ]; then
    echo "required sync-engine metadata is missing or unsafe: $resource" >&2
    exit 1
  fi
done

python3 - "$engine_manifest" "$guardian" "$worker" <<'PY'
import hashlib
import json
import pathlib
import re
import sys

manifest_path, guardian_path, worker_path = map(pathlib.Path, sys.argv[1:])
try:
    raw = manifest_path.read_bytes()
    if len(raw) > 16_384:
        raise ValueError
    def reject_duplicate_pairs(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError
            value[key] = item
        return value
    data = json.loads(raw, object_pairs_hook=reject_duplicate_pairs)
except (OSError, ValueError, json.JSONDecodeError):
    raise SystemExit("sync-engine manifest is invalid") from None

if set(data) != {"schema", "engine", "guardian", "notices", "executables"} or data["schema"] != 1:
    raise SystemExit("sync-engine manifest schema is invalid")
if data["engine"] != {
    "name": "Syncthing",
    "version": "v2.1.3",
    "commit": "946e2b83a1f6c6ae119427c09e0a5802940b82ff",
    "archiveSha256": "e0f0d8df05bf0118c48c6515214a96bf3a3f11dbd115f56c3c0b52251b3f71aa",
    "unsignedExecutableSha256": "6743a0efbf9d39784c7fe2e925505cd0a839458346444aa28620341e47ba12b8",
}:
    raise SystemExit("sync-engine manifest identity is invalid")
if data["guardian"] != {
    "sourceSha256": "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579"
}:
    raise SystemExit("sync-engine guardian provenance is invalid")
expected_notices = {
    "PROVENANCE.txt": "dd8b63bb770c8fc9e6d0c151e575def944e1502f81a651e7ec72588951ebacd9",
    "Syncthing-AUTHORS.txt": "2c197afd6113ec13ae134a20ce8f101d9812b078c3ef32c42c3bb4cdaf2b96a2",
    "Syncthing-LICENSE.txt": "9221c2f936159b8446d329249fb4c0f25be510f447383a0f13336ac7985668a3",
}
if data["notices"] != expected_notices:
    raise SystemExit("sync-engine notice inventory is invalid")
for name, expected in expected_notices.items():
    if hashlib.sha256((manifest_path.parent / name).read_bytes()).hexdigest() != expected:
        raise SystemExit("sync-engine notice digest is invalid")
executables = data["executables"]
if set(executables) != {"covalent-engine-guardian", "covalent-syncthing"}:
    raise SystemExit("sync-engine executable inventory is invalid")
for name, path in (("covalent-engine-guardian", guardian_path), ("covalent-syncthing", worker_path)):
    record = executables[name]
    if set(record) != {"architecture", "signedSha256"} or record["architecture"] != "arm64":
        raise SystemExit("sync-engine executable record is invalid")
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if not re.fullmatch(r"[0-9a-f]{64}", record["signedSha256"]) or record["signedSha256"] != digest:
        raise SystemExit("sync-engine signed executable digest is invalid")
PY

worker_dependencies=$(otool -L "$worker" | tail -n +2 | sed 's/^[[:space:]]*//' | awk '{print $1}' | LC_ALL=C sort)
expected_worker_dependencies='/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation
/System/Library/Frameworks/CoreServices.framework/Versions/A/CoreServices
/System/Library/Frameworks/Security.framework/Versions/A/Security
/usr/lib/libSystem.B.dylib
/usr/lib/libresolv.9.dylib'
if [ "$worker_dependencies" != "$expected_worker_dependencies" ]; then
  echo "bundled sync engine has an unexpected dynamic dependency" >&2
  exit 1
fi
guardian_dependencies=$(otool -L "$guardian" | tail -n +2 | sed 's/^[[:space:]]*//' | awk '{print $1}')
if [ "$guardian_dependencies" != /usr/lib/libSystem.B.dylib ]; then
  echo "bundled engine guardian has an unexpected dynamic dependency" >&2
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$app"
echo "Apple Silicon bundle verified: arm64-only app, helper and pinned sync engine; valid signatures, manifest and inherited sandbox."

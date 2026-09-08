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
  "$engine_resources/Syncthing-LICENSE.txt" "$engine_resources/Syncthing-AUTHORS.txt" \
  "$engine_resources/source-build.json" "$engine_resources/notices-index.txt" \
  "$engine_resources/notices/manifest.json" \
  "$engine_resources/notices/THIRD-PARTY-NOTICES.txt"; do
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
    "upstreamArchiveSha256": "dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959",
    "sourceExportSha256": "eb60efd57d1662af75ffb2f7b89abab7200362c654838486138a34bee00fed29",
    "goVersion": "go1.26.7",
    "unsignedExecutableSha256": "4df1dea892fac3c9d1c0e822827c9a4c57711dcddaaf56a61edaab41d5337bbb",
}:
    raise SystemExit("sync-engine manifest identity is invalid")
if data["guardian"] != {
    "sourceSha256": "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579"
}:
    raise SystemExit("sync-engine guardian provenance is invalid")
expected_notices = {
    "PROVENANCE.txt": "94f3b2bd71120d3dc6f3bdc400a0b538ca8e6be04e740144e140bda4439decc9",
    "Syncthing-AUTHORS.txt": "5a0044d13ddf6f013bdd5c2bc419bf45d6123c356567510237e82f304d113d48",
    "Syncthing-LICENSE.txt": "3f3d9e0024b1921b067d6f7f88deb4a60cbe7a78e76c64e3f1d7fc3b779b9d04",
    "source-build.json": "211b7847de85f74cdf7a9ef4cc19cfd9a6e5a09b8bb68a19307162c8b11f256e",
    "notices-index.txt": "872e47f2495dfaebe7b150f96fbb77d8e8ed5ed7958234f69f566a8daa975dd6",
    "notices/manifest.json": "3624dee064d0ce242d94012c60ffb5aa89b946448166aaf0a613f837bfa3bf5b",
    "notices/THIRD-PARTY-NOTICES.txt": "231a9ded1c9e9f09182187ed2372de5fc2a37c718c4ba67091eac3b9d0bd7a87",
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

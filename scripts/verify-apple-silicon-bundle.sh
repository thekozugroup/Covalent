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
import os
import pathlib
import re
import stat
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
    "source-build.json": "4e603f44b00ed564be92568c6615b7399f29c0a7ad8d1acb49ae094a9ede0cac",
    "notices-index.txt": "34355beb1e337124c8b22f9f2744da16f1911e4e99fffa31060dea0ea2ebd310",
    "notices/manifest.json": "422afb7c2a27e2882e3791dbf8c059e2c96ee529d9ec5773e8cc369f375caa11",
    "notices/THIRD-PARTY-NOTICES.txt": "87e9c362fac963404229addf563d77dfccb23747a8e06a96c5b77d13b675c2fb",
}
if data["notices"] != expected_notices:
    raise SystemExit("sync-engine notice inventory is invalid")
for name, expected in expected_notices.items():
    if hashlib.sha256((manifest_path.parent / name).read_bytes()).hexdigest() != expected:
        raise SystemExit("sync-engine notice digest is invalid")
target_manifest = json.loads(
    (manifest_path.parent / "notices/manifest.json").read_bytes()
)
sources = target_manifest.get("correspondingSources")
if not isinstance(sources, list) or len(sources) != 5:
    raise SystemExit("sync-engine corresponding-source inventory is invalid")
combined = (manifest_path.parent / "notices/THIRD-PARTY-NOTICES.txt").read_text()
for source in sources:
    archive = source.get("archive") if isinstance(source, dict) else None
    relative = archive.get("bundlePath") if isinstance(archive, dict) else None
    expected = archive.get("sha256") if isinstance(archive, dict) else None
    expected_bytes = archive.get("bytes") if isinstance(archive, dict) else None
    if (
        not isinstance(relative, str)
        or re.fullmatch(r"sources/[0-9]{4}-source\.tar\.gz", relative) is None
        or not isinstance(expected, str)
        or re.fullmatch(r"[0-9a-f]{64}", expected) is None
        or not isinstance(expected_bytes, int)
        or isinstance(expected_bytes, bool)
        or not 1 <= expected_bytes <= 64 * 1024 * 1024
        or relative not in combined
        or expected not in combined
    ):
        raise SystemExit("sync-engine corresponding-source record is invalid")
    archive_path = manifest_path.parent / "notices" / relative
    current = manifest_path.parent / "notices"
    for part in pathlib.PurePosixPath(relative).parts:
        current = current / part
        if stat.S_ISLNK(current.lstat().st_mode):
            raise SystemExit("sync-engine corresponding-source path is unsafe")
    descriptor = os.open(
        archive_path,
        os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
    )
    try:
        metadata = os.fstat(descriptor)
        digest = hashlib.sha256()
        retained = 0
        while retained <= expected_bytes:
            chunk = os.read(descriptor, min(1024 * 1024, expected_bytes + 1 - retained))
            if not chunk:
                break
            retained += len(chunk)
            digest.update(chunk)
        final = os.fstat(descriptor)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or retained != expected_bytes
            or digest.hexdigest() != expected
            or (metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns)
            != (final.st_dev, final.st_ino, final.st_size, final.st_mtime_ns)
        ):
            raise SystemExit("sync-engine corresponding-source archive is invalid")
    finally:
        os.close(descriptor)
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

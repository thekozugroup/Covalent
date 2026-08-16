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
if [ ! -d "$app" ] || [ ! -f "$info_plist" ]; then
  echo "Covalent app bundle is missing: $app" >&2
  exit 1
fi

app_executable=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$info_plist")
app_binary="$app/Contents/MacOS/$app_executable"
helper="$app/Contents/MacOS/covalent-node"
require_hardened_runtime=${COVALENT_REQUIRE_HARDENED_RUNTIME:-false}
require_developer_id=${COVALENT_REQUIRE_DEVELOPER_ID:-false}

for binary in "$app_binary" "$helper"; do
  if [ ! -x "$binary" ]; then
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

entitlements=$(codesign -d --entitlements :- "$helper" 2>/dev/null)
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

codesign --verify --deep --strict --verbose=2 "$app"
echo "Apple Silicon bundle verified: arm64-only app and helper, valid signatures, inherited sandbox."

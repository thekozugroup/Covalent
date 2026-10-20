#!/bin/sh
# No installation or credentials. Bind both companion APKs to native provenance
# and to the companion's own resolved dependency/license graph.
set -eu
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
mode=${1:-generate}
case "$mode" in generate|--verify-prebuilt) ;; *) echo 'usage: check-interlink-package.sh [--verify-prebuilt]' >&2; exit 2 ;; esac
sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
tools=${INTERLINK_ANDROID_BUILD_TOOLS:-$sdk/build-tools/37.0.0}
test -x "$tools/aapt2" || { echo 'Set INTERLINK_ANDROID_BUILD_TOOLS to the pinned SDK build-tools directory' >&2; exit 1; }
build="$repo_root/apps/android/companion/build"
config="$repo_root/apps/android/companion/build.gradle.kts"
version=$(sed -n 's/^[[:space:]]*versionName = "\([^"]*\)"$/\1/p' "$config")
code=$(sed -n 's/^[[:space:]]*versionCode = \([0-9]*\)$/\1/p' "$config")
unsigned="$build/outputs/apk/release/companion-release-unsigned.apk"
python3 -B "$repo_root/scripts/interlink-release.py" verify --build-tools "$tools" \
  --apk "$unsigned" --version "$version" --version-code "$code"
COVALENT_ANDROID_ZIPALIGN="$tools/zipalign" "$repo_root/scripts/check-android-native-package.sh" "$unsigned"
COVALENT_ANDROID_ZIPALIGN="$tools/zipalign" "$repo_root/scripts/check-android-native-package.sh" \
  "$build/outputs/bundle/release/companion-release.aab"
python3 - "$unsigned" "$build/outputs/bundle/release/companion-release.aab" <<'PY'
import hashlib, sys, zipfile
def objects(path, prefix):
    result = {}
    with zipfile.ZipFile(path) as archive:
        for info in archive.infolist():
            if not info.filename.startswith(prefix + 'lib/') or info.is_dir():
                continue
            name = info.filename[len(prefix):]
            if name in result or not 0 < info.file_size <= 96 * 1024 * 1024:
                raise SystemExit('Interlink package contains ambiguous or oversized native objects')
            with archive.open(info) as stream:
                result[name] = (info.file_size, hashlib.file_digest(stream, 'sha256').hexdigest())
    return result
apk, bundle = objects(sys.argv[1], ''), objects(sys.argv[2], 'base/')
if not apk or apk != bundle:
    raise SystemExit('Interlink AAB native bytes differ from the verified APK')
print(f'Interlink APK/AAB native byte identity: {len(apk)} objects')
PY
python3 - "$build" <<'PY'
from pathlib import Path
import sys, xml.etree.ElementTree as ET
for variant in ('debug', 'release'):
    report = Path(sys.argv[1]) / f'reports/lint-results-{variant}.xml'
    if not report.is_file() or any(issue.get('severity') in ('Error', 'Fatal') for issue in ET.parse(report).getroot().iter('issue')):
        raise SystemExit(f'Interlink {variant} lint is missing or failed')
print('Interlink debug/release lint reports: no errors')
PY
reports="$repo_root/apps/android/app/build/generated"
summary="$build/reports/interlink/android-native-distribution-summary.json"
set -- --debug-package "$build/outputs/apk/debug/companion-debug.apk" --release-package "$unsigned" \
  --android-sbom "$build/reports/interlink/android-sbom.cdx.json" \
  --android-license-inventory "$build/reports/interlink/android-license-inventory.json" \
  --dependency-verification "$repo_root/apps/android/gradle/verification-metadata.xml"
for abi in arm64-v8a x86_64; do
  set -- "$@" --record "$reports/jniLibs/provenance/jni-link-provenance-$abi.json" \
    --record "$reports/syncEngine/reports/guardian-link-provenance-$abi.json" \
    --record "$reports/syncEngine/reports/rclone-link-provenance-$abi.json"
done
if [ "$mode" = --verify-prebuilt ]; then
  python3 -B "$repo_root/scripts/collect-android-native-distribution-summary.py" "$@" --verify-output "$summary"
else
  work=$(mktemp -d "${TMPDIR:-/tmp}/interlink-package.XXXXXX")
  trap 'rm -rf "$work"' EXIT INT TERM
  python3 -B "$repo_root/scripts/collect-android-native-distribution-summary.py" "$@" --output "$work/summary.json"
  mkdir -p "$(dirname -- "$summary")"
  mv -f "$work/summary.json" "$summary"
fi

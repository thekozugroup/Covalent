#!/bin/sh
# Fast contract test for the guarded personal Android APK entrypoint.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
builder="$repo_root/scripts/build-personal-android-apk.sh"
android_guide="$repo_root/docs/platform/android.md"
developer_guide="$repo_root/apps/android/README.md"

test -x "$builder"
sh -n "$builder"

help_output=$($builder --help)
printf '%s\n' "$help_output" | grep -Fq '[--install DEVICE_SERIAL | --update DEVICE_SERIAL]'
printf '%s\n' "$help_output" | grep -Fq 'never selects a device implicitly'
printf '%s\n' "$help_output" | grep -Fq 'never uninstalls an app'

doctor_call="\"\$repo_root/scripts/setup-doctor.sh\" android"
new_install_call="adb -s \"\$device_serial\" install \"\$artifact\""
update_call="adb -s \"\$device_serial\" install -r \"\$artifact\""
jni_mismatch="packaged JNI library for \$abi does not match"
grep -Fq "$doctor_call" "$builder"
grep -Fq 'scripts/release-version.sh" check' "$builder"
grep -Fq -- '-PcovalentBuildNative=true' "$builder"
grep -Fq 'assembleDebug' "$builder"
grep -Fq 'apksigner" verify --verbose --print-certs' "$builder"
grep -Fq 'dump packagename' "$builder"
grep -Fq 'dump badging' "$builder"
grep -Fq 'expected_version_name' "$builder"
grep -Fq 'expected_version_code' "$builder"
grep -Fq 'zipalign" -c -P 16 4' "$builder"
grep -Fq 'for abi in arm64-v8a x86_64' "$builder"
grep -Fq "$jni_mismatch" "$builder"
grep -Fq 'artifacts/install' "$builder"
grep -Fq 'check-ignore -q "artifacts/install/.ignore-check"' "$builder"
grep -Fq 'refusing symlink artifact directory' "$builder"
grep -Fq 'refusing symlink artifact path' "$builder"
grep -Fq 'publish_without_overwrite' "$builder"
grep -Fq "$new_install_call" "$builder"
grep -Fq "$update_call" "$builder"
grep -Fq 'emulator-5570' "$builder"
grep -Fq 'Covalent_API_37' "$builder"

if grep -Eq 'assembleRelease|app-release-unsigned\.apk' "$builder"; then
  echo "personal Android builder references a release or unsigned APK" >&2
  exit 1
fi

grep -Fq './scripts/build-personal-android-apk.sh' "$android_guide"
grep -Fq 'app-debug.apk' "$android_guide"
grep -Fq -- '--install DEVICE_SERIAL' "$android_guide"
grep -Fq -- '--update DEVICE_SERIAL' "$android_guide"
grep -Fq 'different signers' "$android_guide"
grep -Fq 'app-release-unsigned.apk' "$android_guide"
grep -Fq './scripts/build-personal-android-apk.sh' "$developer_guide"

if "$builder" --install >/dev/null 2>&1; then
  echo "personal Android builder accepted --install without a serial" >&2
  exit 1
fi
if "$builder" --update 'bad serial' >/dev/null 2>&1; then
  echo "personal Android builder accepted an unsafe device serial" >&2
  exit 1
fi

echo "Personal Android APK builder contract: ok"

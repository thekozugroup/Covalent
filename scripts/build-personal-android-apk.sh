#!/bin/sh
# Build, verify, and package the installable personal-use Android debug APK.
set -eu

export LC_ALL=C

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
sdk_version=37.0.0
ndk_version=27.1.12297006
package_name=life.michaelwong.covalent
output_dir="$repo_root/artifacts/install"
install_action=""
device_serial=""
temporary_file=""
work_directory=""

usage() {
  cat <<'EOF'
usage: ./scripts/build-personal-android-apk.sh [--install DEVICE_SERIAL | --update DEVICE_SERIAL]

With no flag, this command only builds, verifies, and packages the APK and evidence.
  --install SERIAL  install on one exact, connected device as a new app
  --update SERIAL   update one exact device; requires the same debug signing key

The command never selects a device implicitly and never uninstalls an app.
EOF
}

fail() {
  echo "personal Android APK: $*" >&2
  exit 1
}

cleanup() {
  if [ -n "$temporary_file" ] && [ -f "$temporary_file" ]; then
    rm -f "$temporary_file"
  fi
  if [ -n "$work_directory" ] && [ -d "$work_directory" ]; then
    rm -rf "$work_directory"
  fi
}
trap cleanup EXIT HUP INT TERM

case "${1:-}" in
  "") ;;
  --help|-h)
    usage
    exit 0
    ;;
  --install|--update)
    install_action=${1#--}
    device_serial=${2:-}
    [ "$#" -eq 2 ] || {
      usage >&2
      exit 64
    }
    [ -n "$device_serial" ] || {
      usage >&2
      exit 64
    }
    ;;
  *)
    usage >&2
    exit 64
    ;;
esac

case "$device_serial" in
  *[!A-Za-z0-9._:-]*)
    fail "device serial contains unsupported characters: $device_serial"
    ;;
esac

android_sdk_path() {
  if [ -n "${ANDROID_SDK_ROOT:-}" ]; then
    printf '%s\n' "$ANDROID_SDK_ROOT"
  elif [ -n "${ANDROID_HOME:-}" ]; then
    printf '%s\n' "$ANDROID_HOME"
  elif [ -d "${HOME}/Library/Android/sdk" ]; then
    printf '%s\n' "${HOME}/Library/Android/sdk"
  else
    printf '%s\n' ""
  fi
}

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    fail "install shasum or sha256sum"
  fi
}

sha256_stdin() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{ print $1 }'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{ print $1 }'
  else
    fail "install shasum or sha256sum"
  fi
}

require_regular_bounded() {
  evidence_label=$1
  evidence_path=$2
  evidence_maximum=$3
  [ -f "$evidence_path" ] && [ ! -L "$evidence_path" ] || {
    fail "$evidence_label is missing or unsafe: $evidence_path"
  }
  evidence_bytes=$(wc -c < "$evidence_path" | tr -d '[:space:]')
  case "$evidence_bytes" in
    ''|*[!0-9]*) fail "could not measure $evidence_label" ;;
  esac
  [ "$evidence_bytes" -gt 0 ] && [ "$evidence_bytes" -le "$evidence_maximum" ] || {
    fail "$evidence_label is empty or exceeds $evidence_maximum bytes"
  }
}

# Publish by creating a new hard link to a private temporary copy. `ln` fails
# when the destination exists, so this function cannot overwrite an artifact.
publish_without_overwrite() {
  source_file=$1
  destination=$2

  [ ! -L "$destination" ] || fail "refusing symlink artifact path: $destination"
  if [ -e "$destination" ]; then
    if [ ! -f "$destination" ] || ! cmp -s "$source_file" "$destination"; then
      fail "refusing to overwrite existing artifact: $destination"
    fi
    echo "  reuse: $destination"
    return
  fi

  temporary_file=$(mktemp "$output_dir/.personal-android.XXXXXX")
  cp "$source_file" "$temporary_file"
  chmod 0644 "$temporary_file"
  if ln "$temporary_file" "$destination" 2>/dev/null; then
    rm -f "$temporary_file"
    temporary_file=""
    echo "  wrote: $destination"
    return
  fi

  if [ -f "$destination" ] && cmp -s "$source_file" "$destination"; then
    rm -f "$temporary_file"
    temporary_file=""
    echo "  reuse: $destination"
    return
  fi
  fail "refusing to overwrite existing artifact: $destination"
}

check_exact_device() {
  serial=$1
  command -v adb >/dev/null 2>&1 || fail "adb is required for --$install_action"
  state=$(adb -s "$serial" get-state 2>/dev/null) || {
    fail "device $serial is not connected and authorized; run: adb devices"
  }
  [ "$state" = "device" ] || fail "device $serial is not ready (state: $state)"

  case "$serial" in
    emulator-*)
      [ "$serial" = "emulator-5570" ] || {
        fail "emulator installs are restricted to emulator-5570 running Covalent_API_37"
      }
      avd_name=$(adb -s "$serial" emu avd name 2>/dev/null | tr -d '\r' | sed -n '1p')
      [ "$avd_name" = "Covalent_API_37" ] || {
        fail "emulator-5570 is running ${avd_name:-an unknown AVD}, not Covalent_API_37"
      }
      ;;
  esac
}

# Fail before a long build when the caller requested a missing or wrong device.
if [ -n "$install_action" ]; then
  check_exact_device "$device_serial"
fi

echo "1/4 Check Android build prerequisites"
"$repo_root/scripts/setup-doctor.sh" android
"$repo_root/scripts/release-version.sh" check

for command_name in awk cmp grep jq python3 sed tar tr unzip; do
  command -v "$command_name" >/dev/null 2>&1 || fail "$command_name is required"
done
command -v git >/dev/null 2>&1 || fail "git is required to verify ignored artifact output"
git -C "$repo_root" check-ignore -q "artifacts/install/.ignore-check" || {
  fail "artifacts/install is not ignored by Git; refusing to create install artifacts"
}
for artifact_directory in "$repo_root/artifacts" "$output_dir"; do
  [ ! -L "$artifact_directory" ] || {
    fail "refusing symlink artifact directory: $artifact_directory"
  }
done

android_sdk=$(android_sdk_path)
[ -n "$android_sdk" ] || fail "set ANDROID_SDK_ROOT or ANDROID_HOME"
build_tools="$android_sdk/build-tools/$sdk_version"
apksigner="$build_tools/apksigner"
aapt2="$build_tools/aapt2"
zipalign="$build_tools/zipalign"
for tool in "$apksigner" "$aapt2" "$zipalign"; do
  [ -x "$tool" ] || fail "required Android Build Tools executable is missing: $tool"
done

export ANDROID_HOME="$android_sdk"
export ANDROID_SDK_ROOT="$android_sdk"
export COVALENT_ANDROID_NDK_HOME="$android_sdk/ndk/$ndk_version"

command -v go >/dev/null 2>&1 || fail "install Go 1.26.7 to build the folder sync engine"
test -f "$repo_root/packaging/rclone/go.mod" || fail "restricted rclone module is missing"

mkdir -p "$output_dir"
work_directory=$(mktemp -d "${TMPDIR:-/tmp}/covalent-personal-android.XXXXXX")
source_before="$work_directory/source-before.txt"
source_after="$work_directory/source-after.txt"
android_build_inputs="apps/android crates packaging/sync-engine packaging/rclone Cargo.toml Cargo.lock rust-toolchain.toml LICENSE scripts/build-android-jni.sh scripts/build-android-sync-engine.sh scripts/android-go-link-wrapper.sh scripts/collect-go-target-license-inventory.py scripts/collect-sync-engine-notices.py scripts/collect-android-native-link-provenance.py scripts/android-source-fingerprint.sh scripts/build-personal-android-apk.sh scripts/android-native-budgets.sh scripts/check-android-native-package.sh"
# These paths are controlled words without spaces. Keep this list aligned with
# the build commands above; the manifest records every tracked and untracked,
# nonignored input by path, mode, and content and double-reads the tree.
# shellcheck disable=SC2086
"$repo_root/scripts/android-source-fingerprint.sh" "$repo_root" $android_build_inputs > "$source_before"
source_commit=$(git -C "$repo_root" rev-parse HEAD)

android_sbom="$repo_root/apps/android/app/build/reports/covalent/android-sbom.cdx.json"
android_licenses="$repo_root/apps/android/app/build/reports/covalent/android-license-inventory.json"
for generated_report in "$android_sbom" "$android_licenses"; do
  [ ! -L "$generated_report" ] || fail "refusing symlinked generated evidence: $generated_report"
  rm -f "$generated_report"
done

echo "2/4 Build the debug-signed APK and both native ABIs"
"$repo_root/apps/android/gradlew" -p "$repo_root/apps/android" --no-daemon \
  --dependency-verification=strict \
  -PcovalentBuildNative=true \
  -PcovalentBuildSyncEngine=true \
  assembleDebug generateAndroidSbom

# shellcheck disable=SC2086
"$repo_root/scripts/android-source-fingerprint.sh" "$repo_root" $android_build_inputs > "$source_after"
cmp -s "$source_before" "$source_after" || fail "Android source changed during the build"

apk="$repo_root/apps/android/app/build/outputs/apk/debug/app-debug.apk"
[ -f "$apk" ] || fail "Gradle did not create app-debug.apk"
case "$apk" in
  */debug/app-debug.apk) ;;
  *) fail "refusing unexpected or unsigned release artifact: $apk" ;;
esac

echo "3/4 Verify signer, package, version, and native libraries"
signer_report=$("$apksigner" verify --verbose --print-certs "$apk" 2>&1) || {
  printf '%s\n' "$signer_report" >&2
  fail "APK signature verification failed"
}
printf '%s\n' "$signer_report"
printf '%s\n' "$signer_report" | grep -Fq 'Number of signers: 1' || {
  fail "personal APK must have exactly one signer"
}
printf '%s\n' "$signer_report" | grep -Fq 'CN=Android Debug' || {
  fail "APK is not signed by the local Android debug identity"
}
certificate_sha256=$(printf '%s\n' "$signer_report" | sed -n \
  's/^.*certificate SHA-256 digest: \([0-9a-fA-F][0-9a-fA-F]*\)$/\1/p' | sed -n '1p')
[ "${#certificate_sha256}" -eq 64 ] || fail "could not read the debug certificate SHA-256"

actual_package=$("$aapt2" dump packagename "$apk")
[ "$actual_package" = "$package_name" ] || {
  fail "unexpected package: $actual_package (expected $package_name)"
}
package_line=$("$aapt2" dump badging "$apk" | sed -n '1p')
actual_version_name=$(printf '%s\n' "$package_line" | sed -n "s/^.*versionName='\([^']*\)'.*$/\1/p")
actual_version_code=$(printf '%s\n' "$package_line" | sed -n "s/^.*versionCode='\([^']*\)'.*$/\1/p")
gradle_file="$repo_root/apps/android/app/build.gradle.kts"
expected_version_name=$(awk -F'"' '/^[[:space:]]*versionName = "/ { print $2; exit }' "$gradle_file")
expected_version_code=$(awk '/^[[:space:]]*versionCode = / { print $3; exit }' "$gradle_file")
[ "$actual_version_name" = "$expected_version_name" ] || {
  fail "APK versionName $actual_version_name does not match source $expected_version_name"
}
[ "$actual_version_code" = "$expected_version_code" ] || {
  fail "APK versionCode $actual_version_code does not match source $expected_version_code"
}

"$zipalign" -c -P 16 4 "$apk" || fail "APK alignment verification failed"
COVALENT_ANDROID_ZIPALIGN="$zipalign" "$repo_root/scripts/check-android-native-package.sh" "$apk"
for abi in arm64-v8a x86_64; do
  entry="lib/$abi/libcovalent_android_jni.so"
  entry_count=$(unzip -Z1 "$apk" | grep -Fxc "$entry" || true)
  [ "$entry_count" -eq 1 ] || {
    fail "APK must contain exactly one $entry (found $entry_count)"
  }
  generated_library="$repo_root/apps/android/app/build/generated/jniLibs/$abi/libcovalent_android_jni.so"
  [ -f "$generated_library" ] || fail "built JNI library is missing: $generated_library"
  generated_sha256=$(sha256_file "$generated_library")
  packaged_sha256=$(unzip -p "$apk" "$entry" | sha256_stdin)
  [ "$packaged_sha256" = "$generated_sha256" ] || {
    fail "packaged JNI library for $abi does not match the source-built library"
  }
  echo "  $abi JNI: $packaged_sha256"
done

sync_engine_root="$repo_root/apps/android/app/build/generated/syncEngine"
notices="$sync_engine_root/assets/sync-engine-notices/THIRD-PARTY-NOTICES.txt"
notice_manifest="$sync_engine_root/assets/sync-engine-notices/manifest.json"
notice_index="$sync_engine_root/assets/sync-engine-notices-index.txt"
rclone_manifest="$sync_engine_root/assets/rclone-sha256.txt"
guardian_manifest="$sync_engine_root/assets/engine-guardian-sha256.txt"
native_source_build="$sync_engine_root/reports/source-build.json"
require_regular_bounded "Android SBOM" "$android_sbom" $((8 * 1024 * 1024))
require_regular_bounded "Android license inventory" "$android_licenses" $((8 * 1024 * 1024))
require_regular_bounded "Android notices" "$notices" $((8 * 1024 * 1024))
require_regular_bounded "Android notice manifest" "$notice_manifest" $((4 * 1024 * 1024))
require_regular_bounded "Android notice index" "$notice_index" 1024
require_regular_bounded "rclone package manifest" "$rclone_manifest" 4096
require_regular_bounded "guardian package manifest" "$guardian_manifest" 4096
require_regular_bounded "native source-build record" "$native_source_build" $((1024 * 1024))

packaged_notices_sha256=$(unzip -p "$apk" assets/sync-engine-notices/THIRD-PARTY-NOTICES.txt | sha256_stdin)
[ "$packaged_notices_sha256" = "$(sha256_file "$notices")" ] || {
  fail "packaged notices differ from the generated notice bundle"
}
packaged_notice_manifest_sha256=$(unzip -p "$apk" assets/sync-engine-notices/manifest.json | sha256_stdin)
[ "$packaged_notice_manifest_sha256" = "$(sha256_file "$notice_manifest")" ] || {
  fail "packaged notice manifest differs from its producer output"
}

echo "4/4 Copy the verified APK and publication evidence without overwriting"
apk_sha256=$(sha256_file "$apk")
[ "${#apk_sha256}" -eq 64 ] || fail "could not calculate the APK SHA-256"
case "$expected_version_name" in
  ''|*[!A-Za-z0-9._-]*) fail "unsafe versionName in source: $expected_version_name" ;;
esac
artifact_name="Covalent-v${expected_version_name}-android-personal-debug-$(printf '%.16s' "$apk_sha256").apk"
artifact="$output_dir/$artifact_name"
checksum="$artifact.sha256"
artifact_stem=${artifact_name%.apk}
sbom_name="${artifact_stem}-SBOM.cdx.json"
licenses_name="${artifact_stem}-license-inventory.json"
notices_name="${artifact_stem}-THIRD-PARTY-NOTICES.txt"
notice_manifest_name="${artifact_stem}-notices-manifest.json"
receipt_name="${artifact_stem}-build-receipt.json"
sbom_artifact="$output_dir/$sbom_name"
licenses_artifact="$output_dir/$licenses_name"
notices_artifact="$output_dir/$notices_name"
notice_manifest_artifact="$output_dir/$notice_manifest_name"
receipt_artifact="$output_dir/$receipt_name"

checksum_stage="$work_directory/$artifact_name.sha256"
printf '%s  %s\n' "$apk_sha256" "$artifact_name" > "$checksum_stage"
sbom_stage="$work_directory/$sbom_name"
licenses_stage="$work_directory/$licenses_name"
notices_stage="$work_directory/$notices_name"
notice_manifest_stage="$work_directory/$notice_manifest_name"
receipt_stage="$work_directory/$receipt_name"
cp "$android_sbom" "$sbom_stage"
cp "$android_licenses" "$licenses_stage"
cp "$notices" "$notices_stage"
cp "$notice_manifest" "$notice_manifest_stage"
chmod 0644 "$checksum_stage" "$sbom_stage" "$licenses_stage" "$notices_stage" "$notice_manifest_stage"

python3 - "$receipt_stage" "$repo_root" "$work_directory" "$apk" "$source_after" \
  "$source_commit" "$expected_version_name" "$expected_version_code" \
  "$package_name" "$certificate_sha256" <<'PY'
import hashlib
import json
import os
import pathlib
import sys

(
    output, repo_root, stage_root, apk_path, source_fingerprint, commit,
    version_name, version_code, package_name, certificate_sha,
) = sys.argv[1:]
repo = pathlib.Path(repo_root)
stage = pathlib.Path(stage_root)
apk = pathlib.Path(apk_path)

def descriptor(path, name=None):
    value = pathlib.Path(path)
    if not value.is_file() or value.is_symlink():
        raise SystemExit(f"personal Android evidence is missing or unsafe: {value}")
    data = value.read_bytes()
    if not data:
        raise SystemExit(f"personal Android evidence is empty: {value}")
    result = {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    if name is not None:
        result["fileName"] = name
    return result

apk_sha = hashlib.sha256(apk.read_bytes()).hexdigest()
artifact_stem = f"Covalent-v{version_name}-android-personal-debug-{apk_sha[:16]}"
status_lines = [
    line for line in pathlib.Path(source_fingerprint).read_text(encoding="ascii").splitlines()
    if line.startswith("status-z\t")
]
if len(status_lines) != 1:
    raise SystemExit("Android source fingerprint has no unique status record")
source_clean = status_lines[0] == "status-z\t"
sync_engine = repo / "apps/android/app/build/generated/syncEngine"
generated_jni = repo / "apps/android/app/build/generated/jniLibs"

receipt = {
    "schemaVersion": 1,
    "status": "build-verified-device-test-required",
    "source": {
        "commit": commit,
        "clean": source_clean,
        "cleanScope": "Android build fingerprint inputs",
        "androidBuildFingerprint": descriptor(source_fingerprint),
    },
    "application": {
        "packageName": package_name,
        "versionName": version_name,
        "versionCode": int(version_code),
        "apk": descriptor(apk, f"{artifact_stem}.apk"),
    },
    "signing": {
        "kind": "Android Debug",
        "signers": 1,
        "certificateSha256": certificate_sha.lower(),
    },
    "native": {
        "jni": {
            "arm64-v8a": descriptor(generated_jni / "arm64-v8a/libcovalent_android_jni.so"),
            "x86_64": descriptor(generated_jni / "x86_64/libcovalent_android_jni.so"),
        },
        "rcloneManifest": descriptor(sync_engine / "assets/rclone-sha256.txt"),
        "guardianManifest": descriptor(sync_engine / "assets/engine-guardian-sha256.txt"),
        "noticeIndex": descriptor(sync_engine / "assets/sync-engine-notices-index.txt"),
        "sourceBuild": descriptor(sync_engine / "reports/source-build.json"),
    },
    "publicationEvidence": {
        "sbom": descriptor(stage / f"{artifact_stem}-SBOM.cdx.json", f"{artifact_stem}-SBOM.cdx.json"),
        "licenseInventory": descriptor(stage / f"{artifact_stem}-license-inventory.json", f"{artifact_stem}-license-inventory.json"),
        "notices": descriptor(stage / f"{artifact_stem}-THIRD-PARTY-NOTICES.txt", f"{artifact_stem}-THIRD-PARTY-NOTICES.txt"),
        "noticeManifest": descriptor(stage / f"{artifact_stem}-notices-manifest.json", f"{artifact_stem}-notices-manifest.json"),
    },
    "deviceTest": {
        "status": "required-separately",
        "apkSha256": apk_sha,
    },
}
encoded = (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode()
fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "wb") as stream:
    stream.write(encoded)
    stream.flush()
    os.fsync(stream.fileno())
PY
chmod 0644 "$receipt_stage"

publish_without_overwrite "$apk" "$artifact"
publish_without_overwrite "$checksum_stage" "$checksum"
publish_without_overwrite "$sbom_stage" "$sbom_artifact"
publish_without_overwrite "$licenses_stage" "$licenses_artifact"
publish_without_overwrite "$notices_stage" "$notices_artifact"
publish_without_overwrite "$notice_manifest_stage" "$notice_manifest_artifact"
publish_without_overwrite "$receipt_stage" "$receipt_artifact"

(
  cd "$output_dir"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$(basename "$checksum")"
  else
    sha256sum -c "$(basename "$checksum")"
  fi
)

printf '\nPersonal APK ready.\n'
printf 'APK: %s\n' "$artifact"
printf 'SHA-256: %s\n' "$apk_sha256"
printf 'Debug certificate SHA-256: %s\n' "$certificate_sha256"
printf 'SBOM: %s\n' "$sbom_artifact"
printf 'License inventory: %s\n' "$licenses_artifact"
printf 'Notices: %s\n' "$notices_artifact"
printf 'Notice manifest: %s\n' "$notice_manifest_artifact"
printf 'Build receipt: %s\n' "$receipt_artifact"
printf '%s\n' 'Device verification for this exact APK remains required before publication.'

if [ -z "$install_action" ]; then
  echo "No device was changed."
fi

case "$install_action" in
  install)
    echo "Installing as a new app on exact target $device_serial"
    if ! adb -s "$device_serial" install "$artifact"; then
      echo "Install failed. This script did not uninstall or erase anything." >&2
      echo "Use --update only when the installed app uses this same debug key." >&2
      exit 1
    fi
    ;;
  update)
    echo "Update rule: the installed app must use this same debug key."
    echo "A build from another computer normally uses a different key. This script never uninstalls."
    if ! adb -s "$device_serial" install -r "$artifact"; then
      echo "Update failed. This script did not uninstall or erase anything." >&2
      echo "Do not uninstall until protected backups pass a restore check." >&2
      exit 1
    fi
    ;;
esac

if [ -n "$install_action" ]; then
  adb -s "$device_serial" shell pm path "$package_name" >/dev/null || {
    fail "Android did not report the installed package $package_name"
  }
  echo "Installed package verified on $device_serial."
fi

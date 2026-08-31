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

usage() {
  cat <<'EOF'
usage: ./scripts/build-personal-android-apk.sh [--install DEVICE_SERIAL | --update DEVICE_SERIAL]

With no flag, this command only builds, verifies, and packages the APK.
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

for command_name in awk cmp grep jq sed tar tr unzip; do
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

echo "2/4 Build the debug-signed APK and both native ABIs"
"$repo_root/apps/android/gradlew" -p "$repo_root/apps/android" --no-daemon \
  --dependency-verification=strict \
  -PcovalentBuildNative=true \
  assembleDebug

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

echo "4/4 Copy the verified APK and checksum without overwriting"
apk_sha256=$(sha256_file "$apk")
[ "${#apk_sha256}" -eq 64 ] || fail "could not calculate the APK SHA-256"
case "$expected_version_name" in
  ''|*[!A-Za-z0-9._-]*) fail "unsafe versionName in source: $expected_version_name" ;;
esac
mkdir -p "$output_dir"
artifact_name="Covalent-v${expected_version_name}-android-personal-debug-$(printf '%.16s' "$apk_sha256").apk"
artifact="$output_dir/$artifact_name"
checksum="$artifact.sha256"

publish_without_overwrite "$apk" "$artifact"
checksum_stage=$(mktemp "$output_dir/.personal-android-checksum.XXXXXX")
printf '%s  %s\n' "$apk_sha256" "$artifact_name" > "$checksum_stage"
publish_without_overwrite "$checksum_stage" "$checksum"
rm -f "$checksum_stage"
temporary_file=""

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

#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
android_sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
avd_name=Covalent_API_37
serial=""

if [ -z "$android_sdk" ] && [ -d "${HOME}/Library/Android/sdk" ]; then
  android_sdk="${HOME}/Library/Android/sdk"
fi
if [ -z "$android_sdk" ]; then
  echo "Android SDK not found; set ANDROID_HOME or ANDROID_SDK_ROOT." >&2
  exit 1
fi

adb="$android_sdk/platform-tools/adb"
if [ ! -x "$adb" ]; then
  echo "adb is missing from $android_sdk/platform-tools." >&2
  exit 1
fi
if ! command -v mobilecli >/dev/null 2>&1; then
  echo "mobilecli is required for headed API 37 evidence." >&2
  exit 1
fi

for candidate in $("$adb" devices | awk 'NR > 1 && $2 == "device" { print $1 }'); do
  case "$candidate" in
    emulator-*)
      candidate_name=$("$adb" -s "$candidate" emu avd name 2>/dev/null | sed -n '1p' | tr -d '\r')
      if [ "$candidate_name" = "$avd_name" ]; then
        if [ -n "$serial" ]; then
          echo "More than one running $avd_name emulator was found." >&2
          exit 1
        fi
        serial=$candidate
      fi
      ;;
  esac
done

if [ -z "$serial" ]; then
  echo "Start the exact $avd_name emulator before running this gate." >&2
  echo "Available mobilecli devices:" >&2
  mobilecli devices --platform android --include-offline >&2 || true
  exit 1
fi

api_level=$("$adb" -s "$serial" shell getprop ro.build.version.sdk | tr -d '\r')
if [ "$api_level" != "37" ]; then
  echo "$avd_name is running API $api_level; API 37 is required." >&2
  exit 1
fi

mobilecli device info --device "$avd_name"
"$repo_root/scripts/check-android.sh"
env \
  ANDROID_HOME="$android_sdk" \
  ANDROID_SDK_ROOT="$android_sdk" \
  ANDROID_SERIAL="$serial" \
  "$repo_root/apps/android/gradlew" \
  -p "$repo_root/apps/android" \
  --no-daemon \
  connectedDebugAndroidTest

apk="$repo_root/apps/android/app/build/outputs/apk/debug/app-debug.apk"
mobilecli apps install "$apk" --device "$avd_name"
if ! mobilecli apps launch life.michaelwong.covalent --device "$avd_name"; then
  echo "mobilecli launch backend is incompatible with this API 37 image; using explicit adb lifecycle fallback." >&2
  "$adb" -s "$serial" shell am start -W -n life.michaelwong.covalent/.MainActivity
fi

evidence_dir=${COVALENT_ANDROID_EVIDENCE_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/covalent-api37.XXXXXX")}
mkdir -p "$evidence_dir"
mobilecli screenshot \
  --device "$avd_name" \
  --format png \
  --output "$evidence_dir/first-launch.png"
mobilecli dump ui --device "$avd_name" > "$evidence_dir/first-launch-ui.txt"

"$adb" -s "$serial" reverse tcp:8787 tcp:8787
echo "API 37 device gate passed on $serial ($avd_name)."
echo "Evidence: $evidence_dir"
echo "ADB reverse is ready: Android http://127.0.0.1:8787 -> host 127.0.0.1:8787."

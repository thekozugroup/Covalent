#!/bin/sh
set -eu

proof_root=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
serial=${ANDROID_SERIAL:-emulator-5570}
prebuilt=false
if test "${1:-}" = "--prebuilt"; then
  prebuilt=true
  shift
fi
test "$#" = 0 || {
  echo "usage: ANDROID_SERIAL=serial $0 [--prebuilt]" >&2
  exit 2
}

adb_bin=${ANDROID_HOME:?Set ANDROID_HOME}/platform-tools/adb
test -x "$adb_bin" || { echo "adb is missing: $adb_bin" >&2; exit 1; }
test "$($adb_bin -s "$serial" shell getprop ro.build.version.sdk | tr -d '\r')" = 37 || {
  echo "The proof requires an API 37 device" >&2
  exit 1
}
abilist=$($adb_bin -s "$serial" shell getprop ro.product.cpu.abilist | tr -d '\r')
case ",$abilist," in
  *,x86_64,*) ;;
  *) echo "The hosted runtime proof requires x86_64, found $abilist" >&2; exit 1 ;;
esac

if test "$prebuilt" = false; then
  test -n "${SYNCTHING_SOURCE_DIR:-}" || {
    echo "Set SYNCTHING_SOURCE_DIR to official Syncthing v2.1.3 commit 946e2b8..." >&2
    exit 1
  }
  "$proof_root/gradlew" \
    -p "$proof_root" \
    --no-daemon \
    --dependency-verification=strict \
    -PsyncthingSourceDir="$SYNCTHING_SOURCE_DIR" \
    :app:assembleDebug \
    :app:assembleDebugAndroidTest
fi

app_apk="$proof_root/app/build/outputs/apk/debug/app-debug.apk"
test_apk="$proof_root/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"
test -f "$app_apk" || { echo "Missing proof APK: $app_apk" >&2; exit 1; }
test -f "$test_apk" || { echo "Missing proof test APK: $test_apk" >&2; exit 1; }

cleanup() {
  "$adb_bin" -s "$serial" uninstall life.michaelwong.covalent.engineproof.test >/dev/null 2>&1 || true
  "$adb_bin" -s "$serial" uninstall life.michaelwong.covalent.engineproof >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM
cleanup
"$adb_bin" -s "$serial" install -r -t "$app_apk"
"$adb_bin" -s "$serial" install -r -t "$test_apk"

python3 - "$adb_bin" "$serial" <<'PY'
import subprocess
import sys

command = [
    sys.argv[1], "-s", sys.argv[2], "shell", "am", "instrument", "-w", "-r",
    "-e", "class",
    "life.michaelwong.covalent.engineproof.SyncthingExecutableProofTest",
    "life.michaelwong.covalent.engineproof.test/androidx.test.runner.AndroidJUnitRunner",
]
try:
    completed = subprocess.run(command, capture_output=True, timeout=180, check=False)
except subprocess.TimeoutExpired as error:
    raise SystemExit("instrumentation exceeded the 180-second bound") from error
output = completed.stdout + completed.stderr
if len(output) > 1024 * 1024:
    raise SystemExit("instrumentation output exceeded the 1 MiB bound")
text = output.decode("utf-8", errors="replace")
print(text)
if completed.returncode != 0 or "FAILURES!!!" in text or "OK (1 test)" not in text:
    raise SystemExit("Syncthing executable proof failed")
PY

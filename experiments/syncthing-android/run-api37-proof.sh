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
"$adb_bin" -s "$serial" shell pm enable life.michaelwong.covalent.engineproof.test >/dev/null

python3 - "$adb_bin" "$serial" <<'PY'
import subprocess
import sys
import re

def adb_capture(arguments, timeout=15):
    result = subprocess.run(
        [sys.argv[1], "-s", sys.argv[2], *arguments],
        capture_output=True, timeout=timeout, check=False,
    )
    return result.returncode, result.stdout + result.stderr

def framework_pids():
    code, output = adb_capture(["shell", "ps", "-A", "-o", "PID,NAME"])
    if code != 0 or len(output) > 1024 * 1024:
        raise RuntimeError("Could not inspect bounded Android framework state")
    pairs = [line.split() for line in output.decode("utf-8", errors="replace").splitlines()]
    found = {fields[1]: fields[0] for fields in pairs
             if len(fields) == 2 and fields[1] in ("surfaceflinger", "system_server")}
    if set(found) != {"surfaceflinger", "system_server"} or not all(v.isdigit() for v in found.values()):
        raise RuntimeError("Android framework PIDs were not readable")
    return found

def redact(text):
    return re.sub(r"(?i)[0-9a-f]{64}", "[redacted 256-bit value]", text)

def failure_diagnostics():
    try:
        _, raw = adb_capture(["logcat", "-d", "-b", "crash,system", "-t", "1200"])
        print("Bounded Android failure diagnostics:")
        print(redact(raw[-512 * 1024:].decode("utf-8", errors="replace")))
    except Exception:
        print("Android failure diagnostics unavailable")

command = [
    sys.argv[1], "-s", sys.argv[2], "shell", "am", "instrument", "-w", "-r",
    "-e", "class",
    "life.michaelwong.covalent.engineproof.SyncthingExecutableProofTest",
    "life.michaelwong.covalent.engineproof.test/androidx.test.runner.AndroidJUnitRunner",
]
try:
    before = framework_pids()
    completed = subprocess.run(command, capture_output=True, timeout=180, check=False)
    output = completed.stdout + completed.stderr
    if len(output) > 1024 * 1024:
        raise RuntimeError("instrumentation output exceeded the 1 MiB bound")
    text = output.decode("utf-8", errors="replace")
    print(redact(text))
    expected_tests = {
        "extractedHelperRunsPrivateOfflineApiAndRetainsIdentityAfterColdRestart",
        "guardianOwnsDirectWorkerAndStopsItOnOwnerOrGuardianDeathThenRestarts",
    }
    expected_class = (
        "life.michaelwong.covalent.engineproof.SyncthingExecutableProofTest")
    records = []
    fields = {}
    for raw_line in text.splitlines():
        if raw_line.startswith("INSTRUMENTATION_STATUS: "):
            pair = raw_line[len("INSTRUMENTATION_STATUS: "):].split("=", 1)
            if len(pair) == 2 and pair[0] in {"class", "numtests", "test"}:
                if pair[0] in fields:
                    raise RuntimeError("Duplicate instrumentation status field")
                fields[pair[0]] = pair[1]
        elif raw_line.startswith("INSTRUMENTATION_STATUS_CODE: "):
            try:
                status_code = int(raw_line.split(":", 1)[1].strip())
            except ValueError as error:
                raise RuntimeError("Malformed instrumentation status code") from error
            if "test" in fields:
                records.append((fields, status_code))
            fields = {}

    by_test = {name: [] for name in expected_tests}
    for record, status_code in records:
        name = record.get("test")
        if name not in expected_tests:
            raise RuntimeError("Instrumentation ran an unexpected test method")
        if record.get("class") != expected_class or record.get("numtests") != "2":
            raise RuntimeError("Instrumentation reported an unexpected class or test count")
        by_test[name].append(status_code)

    exact_method_results = all(codes == [1, 0] for codes in by_test.values())
    final_codes = re.findall(r"(?m)^INSTRUMENTATION_CODE: (-?\d+)\r?$", text)
    if (completed.returncode != 0 or "FAILURES!!!" in text or "OK (2 tests)" not in text
            or final_codes != ["-1"] or not exact_method_results):
        raise RuntimeError("Syncthing executable proof failed")
    after = framework_pids()
    if before != after:
        raise RuntimeError("Android framework restarted during instrumentation")
    print("Android framework PIDs remained stable across the proof")
except Exception:
    failure_diagnostics()
    raise
PY

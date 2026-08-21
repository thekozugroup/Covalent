#!/bin/sh
# Block until an emulator guest is genuinely ready to be tested, or explain why
# it never got there.
#
# "Ready" has been redefined three times by this gate, each time because the
# previous definition was satisfied by a device that then failed:
#
#   * `sys.boot_completed=1` alone. Run 32495908942 reached instrumentation two
#     seconds after it and 40 of 56 tests failed, because user 0 was still
#     locked - PackageManager matches only direct-boot-aware components then, so
#     77 activity resolutions failed and every credential-encrypted read threw.
#   * ...plus `sys.user.0.ce_available=true`. Run 32499985083 passed that and
#     died in `adb install` inside StorageManagerService on a null
#     PackageManagerInternal, which is registered into LocalServices some
#     unadvertised time after boot.
#   * ...plus one successful probe install. Run 32506293772 passed *that* - the
#     probe reported "Success" at t+0s - and the gate's own install twelve
#     seconds later died with `cmd: Can't find service: package`. The package
#     service had gone away again. A single success is a sample, not a state.
#
# So this waits for the capability and then requires it to survive. The probe
# install is deliberately performed *before* the stability window rather than
# after it: installing is the heaviest thing that happens to a young guest, it
# is what triggers dexopt, and if that is going to knock a service over then it
# has to knock it over while this script is still watching. A guest that
# installs an APK and still answers for `package`, `activity` and `mount`
# several samples later is ready in a way none of the three definitions above
# could express.
#
# Nothing here asserts anything about Covalent. Establishing a working device is
# the harness's job, and doing it properly is what lets the gate's own failures
# mean something.
set -eu

serial=${1:-}
apk=${2:-}
if [ -z "$serial" ] || [ -z "$apk" ]; then
  echo "usage: wait-for-android-guest.sh <serial> <probe-apk>" >&2
  exit 1
fi

android_sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
if [ -z "$android_sdk" ]; then
  echo "Android SDK not found; set ANDROID_HOME or ANDROID_SDK_ROOT." >&2
  exit 1
fi
adb="$android_sdk/platform-tools/adb"
if [ ! -x "$adb" ]; then
  echo "adb is missing from $android_sdk/platform-tools." >&2
  exit 1
fi
if [ ! -f "$apk" ]; then
  echo "probe APK $apk does not exist." >&2
  exit 1
fi

# Bounded in wall-clock, not in attempts, so a guest that is merely slow is
# given the whole budget while one that is wedged still ends the job.
budget=${COVALENT_ANDROID_GUEST_READY_TIMEOUT:-600}
# Four samples five seconds apart. Long enough to span a system_server restart,
# which takes tens of seconds to drop and re-register its services, and short
# enough that a healthy guest pays twenty seconds for the proof.
stable_samples=${COVALENT_ANDROID_GUEST_STABLE_SAMPLES:-4}
sample_interval=${COVALENT_ANDROID_GUEST_SAMPLE_INTERVAL:-5}
started=$(date +%s)

elapsed() {
  echo $(($(date +%s) - started))
}

out_of_budget() {
  [ "$(elapsed)" -ge "$budget" ]
}

prop_is() {
  [ "$("$adb" -s "$serial" shell getprop "$1" 2>/dev/null | tr -d '\r')" = "$2" ]
}

# Asserted as "not running" rather than "== stopped" on purpose. This AVD boots
# with -no-boot-anim, so init never starts bootanim and init.svc.bootanim is
# absent entirely - getprop answers with an empty string, which is never
# "stopped". Requiring "stopped" here would have burned the whole readiness
# budget on every run and reported a wedged guest that was in fact fine.
prop_is_not() {
  [ "$("$adb" -s "$serial" shell getprop "$1" 2>/dev/null | tr -d '\r')" != "$2" ]
}

# `service check <name>` answers "Service <name>: found" only while the binder
# service is registered, which is exactly the question `Can't find service:
# package` was the answer to.
service_found() {
  "$adb" -s "$serial" shell service check "$1" 2>/dev/null |
    tr -d '\r' | grep -q ': found'
}

# The services the gate actually uses: package for installs, activity for
# `am instrument`, mount for the storage allocation that killed run 32499985083.
guest_is_ready() {
  prop_is sys.boot_completed 1 &&
    prop_is sys.user.0.ce_available true &&
    prop_is_not init.svc.bootanim running &&
    service_found package &&
    service_found activity &&
    service_found mount
}

report_and_fail() {
  echo "API-37-gate: $1 after $(elapsed)s" >&2
  # Every dump below needs a device to answer it, and one of them does not fail
  # fast without one: `adb logcat` on an absent serial blocks indefinitely
  # rather than erroring, which would turn this diagnostic into the job's cause
  # of death. Establish the device is there before asking it anything.
  if [ "$("$adb" -s "$serial" get-state 2>/dev/null || true)" != "device" ]; then
    echo "--- $serial is not connected; no on-device evidence available ---" >&2
    "$adb" devices >&2 2>&1 || true
    exit 1
  fi
  echo "--- properties ---" >&2
  "$adb" -s "$serial" shell getprop 2>&1 |
    grep -E 'sys\.boot_completed|sys\.user\.0|init\.svc\.bootanim|dev\.bootcomplete' >&2 || true
  echo "--- services ---" >&2
  for svc in package activity mount; do
    printf '%s: ' "$svc" >&2
    "$adb" -s "$serial" shell service check "$svc" >&2 2>&1 || true
  done
  echo "--- storage ---" >&2
  "$adb" -s "$serial" shell df /data >&2 2>&1 || true
  echo "--- users ---" >&2
  "$adb" -s "$serial" shell dumpsys user >&2 2>&1 || true
  # The one thing no previous failure captured. If system_server is dying under
  # this gate, its death is in here and every future diagnosis starts from it
  # instead of from inference.
  echo "--- crash and system log tail ---" >&2
  "$adb" -s "$serial" logcat -d -b crash -b system -t 400 >&2 2>&1 || true
  exit 1
}

echo "API-37-gate: waiting for $serial to become ready (budget ${budget}s)"

while : ; do
  out_of_budget && report_and_fail "the guest never reported a ready platform"
  guest_is_ready && break
  sleep "$sample_interval"
done
echo "API-37-gate: platform reported ready after $(elapsed)s"

# Not fatal and not always available: `am wait-for-broadcast-idle` is how AOSP's
# own harnesses wait out the boot broadcast storm, but it is a developer command
# and images vary. When it works it removes the largest remaining source of
# background work; when it does not, the stability window below still has to
# pass on its own.
if "$adb" -s "$serial" shell am wait-for-broadcast-idle >/dev/null 2>&1; then
  echo "API-37-gate: broadcast queues idle after $(elapsed)s"
else
  echo "API-37-gate: am wait-for-broadcast-idle unavailable on this image; continuing"
fi

# Perturb, then prove. Everything above is the guest describing itself; this is
# the guest doing the actual thing the gate needs it to do, followed by the
# demonstration that doing it did not break anything. `-r` makes it idempotent
# and the gate script installs again anyway, so this is a precondition and not
# an assertion - nothing the gate proves is weakened by establishing it.
while : ; do
  out_of_budget && report_and_fail "the guest never accepted a package install"
  if "$adb" -s "$serial" install -r "$apk" >/dev/null 2>&1; then
    break
  fi
  sleep "$sample_interval"
done
echo "API-37-gate: probe install accepted after $(elapsed)s"

settled=0
while [ "$settled" -lt "$stable_samples" ]; do
  out_of_budget && report_and_fail "the guest never stayed ready for $stable_samples consecutive samples"
  sleep "$sample_interval"
  if guest_is_ready; then
    settled=$((settled + 1))
  else
    # A regression here is the run-32506293772 shape: ready, installed, then a
    # service disappears. Start the count over rather than averaging over it.
    if [ "$settled" -ne 0 ]; then
      echo "API-37-gate: the guest stopped being ready at $(elapsed)s; restarting the stability window"
    fi
    settled=0
  fi
done

echo "API-37-gate: guest ready and stable across $stable_samples samples after $(elapsed)s"

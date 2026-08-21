#!/bin/sh
# Block until an emulator guest is genuinely ready to be tested, or explain why
# it never got there.
#
# "Ready" has been redefined once per failure by this gate, each time because
# the previous definition was satisfied by a device that then failed. Every
# definition below is cumulative, and every one of them is still enforced:
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
#   * ...plus user 0 actually being unlocked. Run 32513657537 passed every check
#     above - system_server never even restarted, PID 5198 served the whole run -
#     and 40 of 56 tests still failed the instant they started, 77 of them on
#     `Unable to resolve activity for ... androidx.activity.ComponentActivity`.
#     The guest log names the reason directly: `AccessibilityManagerService:
#     Ignoring non-encryption-aware service`, which AOSP logs only when
#     `isUserUnlockedLocked()` is false. User 0 was LOCKED.
#     `sys.user.0.ce_available` had been `true` since the first poll. That does
#     not make the property a liar - measured locally by setting a PIN,
#     rebooting and never entering it, a genuinely CE-locked guest reports it
#     *empty*, so it does discriminate a locked boot from an unlocked one (the
#     table further down records the measurement). What it cannot do is stay
#     honest about *this* failure: it flips early and then says nothing more,
#     while the unlock the tests depend on lands later. The authoritative answer
#     is the user's own credential-encrypted key set, which `dumpsys mount`
#     prints as `CE unlocked users: [0]`, and `am get-started-user-state 0`
#     corroborates as RUNNING_UNLOCKED. Waiting on that is what the previous
#     three definitions were all reaching for and none of them expressed: every
#     one of those 40 failures - unresolvable non-direct-boot activities,
#     credential-encrypted reads, keystore levels - is the same single fact
#     about the guest.
#   * ...plus all of the above proven stable across four samples. Run
#     32522700913 passed every one of them at t+62s of a 600s budget and the
#     gate's install still died on `Broken pipe (32)`. system_server never
#     restarted - one PID, 5240, served the whole log - it was alive and
#     *paralysed*: `Slow dispatch took 1863ms`, repeated `Long monitor
#     contention` on `OomAdjusterImpl`, lock holds of 1.2s/1.7s/2.0s/2.4s/3.3s
#     inside `UserBackupManagerService.initPackageTracking`, two ~1.9s
#     system_server GCs. The host was saturated (4 vCPU, load 4.18) and the
#     guest's own `/proc/loadavg` read `30.62 9.53 3.37` on two cores.
#
#     Every service check this gate had was `service check <name>`, and that is
#     a *servicemanager handle lookup* - it asks a tiny native daemon whether a
#     binder name is registered and never enters system_server at all. A guest
#     at load 30 answers it in milliseconds. So the gate declared the guest
#     ready while it was mid boot-storm and handed it to an install that
#     immediately failed. A probe that cannot fail when the install fails is not
#     a probe. The checks below therefore make the same round trip the install
#     makes - `cmd package path android`, `cmd activity get-current-user`,
#     `dumpsys mount` all transact into system_server and have to be answered -
#     and additionally require the guest's own one-minute load average to be
#     back under a ceiling derived from its core count, because a guest whose
#     runqueue is fifteen deep per core is not ready no matter what it answers.
#
# So this waits for the capability and then requires it to survive. The probe
# install is deliberately performed *before* the stability window rather than
# after it: installing is the heaviest thing that happens to a young guest, it
# is what triggers dexopt, and if that is going to knock a service over then it
# has to knock it over while this script is still watching. A guest that
# installs an APK and still answers a real round trip to `package`, `activity`
# and `mount` several samples later, at a load its own core count can carry, is
# ready in a way none of the earlier definitions could express.
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
#
# 900s rather than the 600s this used to allow, because the load ceiling below
# is a condition the old definition simply did not have and it takes real time
# to satisfy. `/proc/loadavg`'s one-minute figure is an exponential moving
# average with a 60s time constant, so a guest coming out of a boot storm that
# peaked at 30.62 needs 60*ln(30.62/4) = 121s of quiet before it reads under a
# two-core ceiling of 4.00 - and that is the floor, not the expectation. The
# enclosing job allows 75 minutes and the gate script that follows this one is
# the long pole, so the extra five minutes are affordable; being unable to
# afford them would be an argument for a bigger runner, not for handing the
# install a guest that is still thrashing.
budget=${COVALENT_ANDROID_GUEST_READY_TIMEOUT:-900}
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

# How long one binder round trip is allowed to take before the guest is called
# unready. Ten seconds is four times the longest single lock hold observed in
# run 32522700913 (3.3s in UserBackupManagerService.initPackageTracking), so a
# guest that merely queues behind one storm-era lock still answers, and one that
# cannot answer inside ten seconds is not a guest an `adb install` will survive.
probe_deadline=${COVALENT_ANDROID_GUEST_PROBE_TIMEOUT:-10}

# Deadlines on both sides of adb, because the two sides fail differently.
#
# The guest-side `timeout` (toybox, /system/bin/timeout on this image) is the
# one that matters: it kills a binder transaction that has blocked on a
# system_server lock and lets the shell exit, which is precisely the state being
# probed for. The host-side wrapper covers what the guest-side one cannot - an
# `adb shell` that never establishes or never returns at all - so that a
# readiness probe can never become the reason the job hangs. GNU coreutils
# `timeout` is present on every runner this workflow uses; a developer box
# without it (or without Homebrew's `gtimeout`) still gets the guest-side
# deadline, which is the load-bearing one.
if command -v timeout >/dev/null 2>&1; then
  host_deadline() { timeout "$@"; }
elif command -v gtimeout >/dev/null 2>&1; then
  host_deadline() { gtimeout "$@"; }
else
  host_deadline() { shift; "$@"; }
fi

# Echoes the command's guest-side output, or nothing if either deadline fired.
bounded_shell() {
  host_deadline "$((probe_deadline + 5))" \
    "$adb" -s "$serial" shell timeout "$probe_deadline" "$@" 2>/dev/null |
    tr -d '\r'
}

# The three services the gate actually uses, each probed by making the *same
# kind of round trip the gate itself makes* rather than by asking whether a
# name is registered.
#
# This is the whole correction. `service check package` reaches servicemanager,
# a ~200-line native daemon that owns a name->handle table, and returns
# "Service package: found" the instant that table has an entry. It does not
# transact with PackageManagerService, so no amount of contention inside
# system_server can make it fail - which is why it passed at t+62s in run
# 32522700913 while the install twelve seconds later died on a broken pipe, and
# why it survived six rounds of otherwise-correct fixes to this gate.
#
# Each replacement below resolves the handle *and then* makes a real
# transaction that system_server has to take a lock and answer:
#
#   package   `cmd package path android` - PackageManagerService.getPackageInfo
#             on the framework package; the same service `adb install` shells
#             into, and the exact one that answered `Can't find service:
#             package` in run 32506293772.
#   activity  `cmd activity get-current-user` - ActivityManagerService, which is
#             what `am instrument` calls into, and whose OomAdjusterImpl monitor
#             is what run 32522700913's `Long monitor contention` names.
#   mount     `dumpsys mount` - StorageManagerService, whose null
#             PackageManagerInternal killed run 32499985083's install.
#
# Each is matched on its actual answer, not on its exit status, because `cmd`
# prints its errors to stdout and exits 0 often enough that the status is not
# evidence of anything.
package_service_answers() {
  bounded_shell cmd package path android | grep -q '^package:/'
}

activity_service_answers() {
  bounded_shell cmd activity get-current-user | grep -qE '^[0-9]+$'
}

mount_service_answers() {
  bounded_shell dumpsys mount | grep -q 'CE unlocked users:'
}

# The guest's own core count, so the load ceiling is a statement about this
# guest rather than a constant that quietly stops matching. `cores` in ci.yml
# has already been changed twice - 4, then 2 - and the number of cores is
# exactly what a load average has to be read relative to: a one-minute load of
# 4.0 is a busy-but-fine four-core guest and a two-core guest with its runqueue
# twice as deep as it can serve.
#
# Deliberately not defaulted: a guest that cannot answer `cat /proc/cpuinfo` has
# not got far enough to be measured, and saying so is better than inventing a
# denominator and reporting a ratio against it.
guest_core_count() {
  cores=$(bounded_shell grep -c '^processor' /proc/cpuinfo |
    grep -E '^[1-9][0-9]*$' || true)
  [ -n "$cores" ] || return 1
  echo "$cores"
}

# Multiplier over the core count. Two means "the runqueue may be one job deep
# per core beyond the one running" - room for a guest that is working without
# being a guest that is drowning. Overridable, because the right number is a
# property of the runner and this file should not be edited to find out.
load_multiplier=${COVALENT_ANDROID_GUEST_LOAD_MULTIPLIER:-2}

# The one-minute figure specifically. The five- and fifteen-minute averages
# still carry the boot storm long after it has ended - run 32522700913 read
# `30.62 9.53 3.37`, so its fifteen-minute figure alone would have passed a
# ceiling of 4.00 while the guest was at 30 - and the question here is whether
# the guest is thrashing *now*.
guest_load1() {
  bounded_shell cat /proc/loadavg | awk 'NR==1 { print $1 }' |
    grep -E '^[0-9]+(\.[0-9]+)?$' || true
}

# awk rather than shell arithmetic because load averages are decimals and `sh`
# has only integers; scaling them by hand invites `$((08))`, which is an octal
# parse error and would have made this predicate fail open on a load of x.08.
load_is_under() {
  awk -v load="$1" -v ceiling="$2" 'BEGIN { exit (load <= ceiling) ? 0 : 1 }'
}

# Until user 0's credential-encrypted key is unlocked, PackageManager matches
# only direct-boot-aware components, so every activity the tests launch is
# unresolvable, every credential-encrypted read throws, and keystore refuses to
# name a security level - which is exactly the 40-failure shape run 32513657537
# produced on a guest that was otherwise perfectly healthy.
#
# Read StorageManagerService's credential-encrypted key set first, because that
# is the field the failures actually depend on. `dumpsys mount` prints
# `CE unlocked users: [0]` on an unlocked guest and `CE unlocked users: []` on a
# locked one, and that set is what UserManager.isUserUnlocked() returns and what
# PackageManager's updateFlagsForComponent consults: with the CE key locked an
# unflagged query is narrowed to MATCH_DIRECT_BOOT_AWARE, every non-direct-boot
# component stops resolving, and androidx.test's ActivityInvoker falls back to
# naming the *test* package - which is the `cmp=life.michaelwong.covalent.test/
# androidx.activity.ComponentActivity` shape this gate keeps dying on.
#
# Measured on a local API 37 guest by setting a PIN, rebooting, and never
# entering it, then clearing it again:
#
#                                    CE-locked            unlocked
#   dumpsys mount CE unlocked users  []                   [0]
#   am get-started-user-state 0      RUNNING_LOCKED       RUNNING_UNLOCKED
#   getprop sys.user.0.ce_available  (empty)              true
#   resolve-activity ComponentActivity  No activity found  resolves
#
# So all three discriminate a locked boot from an unlocked one, and the
# `sys.user.0.ce_available` check in the readiness window above is a real check
# and not decoration. What it cannot do is stay honest once the guest is merely
# slow, which is the case below - it is a property init sets once, not a
# question anyone has to be alive to answer.
#
# Ordering matters for reliability, not just correctness. `am
# get-started-user-state` costs an app_process start and on a two-core guest
# under load it sometimes does not answer in time; when it did not, the old
# fallback asked `dumpsys user` for `0=RUNNING_UNLOCKED`, found
# `0=RUNNING_LOCKED` instead, and reported "unknown" - not "locked" - on a guest
# that was definitively locked. `dumpsys mount` is one dumpsys against a service
# the readiness window above already requires, so it answers in the case where
# the expensive probe does not.
#
# Answers "unlocked", "locked" or "unknown", and the third is not the second.
user0_state() {
  ce_line=$("$adb" -s "$serial" shell dumpsys mount 2>/dev/null | tr -d '\r' |
    grep -m1 'CE unlocked users:')
  case "$ce_line" in
    *'CE unlocked users: ['*)
      # Match the id exactly. A substring test for "0" would accept a guest
      # where only user 10 is unlocked.
      ids=${ce_line#*[}
      ids=${ids%%]*}
      for id in $(echo "$ids" | tr ',' ' '); do
        [ "$id" = 0 ] && { echo unlocked; return; }
      done
      echo locked
      return
      ;;
  esac
  case "$("$adb" -s "$serial" shell am get-started-user-state 0 2>/dev/null | tr -d '\r')" in
    *RUNNING_UNLOCKED*) echo unlocked; return ;;
    *RUNNING_LOCKED*|*RUNNING_UNLOCKING*|*BOOTING*) echo locked; return ;;
  esac
  # Anchored for the same reason as the id match above: an unanchored
  # `0=RUNNING_UNLOCKED` also matches `10=RUNNING_UNLOCKED`.
  if "$adb" -s "$serial" shell dumpsys user 2>/dev/null | tr -d '\r' |
    grep -qE '(^|[^0-9])0=RUNNING_UNLOCKED'; then
    echo unlocked
  else
    echo unknown
  fi
}

# Echoes the first condition that is not satisfied, or nothing when the guest is
# ready. Naming it matters: run 32515541756 reset its stability window sixteen
# times in 600s and every message said only "the guest stopped being ready",
# which is the one thing that was already obvious.
#
# Ordered cheapest-first, and that ordering is doing work rather than saving
# milliseconds. A guest mid boot storm fails the load ceiling, which costs one
# `cat` of a kernel file; short-circuiting there keeps each unready sample at
# about a second instead of spending three ten-second binder deadlines learning
# what the load average already said. Once the load is back under the ceiling
# the round trips run, and they are what actually has to hold.
first_unready_reason() {
  prop_is sys.boot_completed 1 || { echo "sys.boot_completed is not 1"; return; }
  prop_is sys.user.0.ce_available true ||
    { echo "sys.user.0.ce_available is not true"; return; }
  prop_is_not init.svc.bootanim running ||
    { echo "init.svc.bootanim is still running"; return; }
  if ! cores=$(guest_core_count); then
    echo "the guest did not answer /proc/cpuinfo within ${probe_deadline}s"
    return
  fi
  ceiling=$((cores * load_multiplier))
  load=$(guest_load1)
  if [ -z "$load" ]; then
    echo "the guest did not answer /proc/loadavg within ${probe_deadline}s"
    return
  fi
  load_is_under "$load" "$ceiling" || {
    echo "guest 1-minute load $load exceeds the ceiling $ceiling for its $cores cores"
    return
  }
  package_service_answers ||
    { echo "PackageManagerService did not answer 'cmd package path android' within ${probe_deadline}s"; return; }
  activity_service_answers ||
    { echo "ActivityManagerService did not answer 'cmd activity get-current-user' within ${probe_deadline}s"; return; }
  mount_service_answers ||
    { echo "StorageManagerService did not answer 'dumpsys mount' within ${probe_deadline}s"; return; }
  echo ""
}

report_and_fail() {
  echo "API-37-gate: giving up after $(elapsed)s: $1" >&2
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
  # Both halves, side by side and labelled, because their disagreement is the
  # diagnosis. A dump showing every handle "found" and every round trip empty is
  # a paralysed system_server and nothing else; showing only one half is how six
  # rounds of this went unresolved.
  echo "--- binder handles registered (servicemanager only) ---" >&2
  for svc in package activity mount; do
    printf '%s: ' "$svc" >&2
    "$adb" -s "$serial" shell service check "$svc" >&2 2>&1 || true
  done
  echo "--- binder round trips into system_server ---" >&2
  printf 'cmd package path android: ' >&2
  bounded_shell cmd package path android >&2 2>&1 || true
  printf 'cmd activity get-current-user: ' >&2
  bounded_shell cmd activity get-current-user >&2 2>&1 || true
  printf 'dumpsys mount CE line: ' >&2
  bounded_shell dumpsys mount 2>/dev/null | grep 'CE unlocked users:' >&2 || true
  echo "--- guest load ---" >&2
  printf '/proc/loadavg: ' >&2
  "$adb" -s "$serial" shell cat /proc/loadavg >&2 2>&1 || true
  printf 'cores: ' >&2
  "$adb" -s "$serial" shell grep -c '^processor' /proc/cpuinfo >&2 2>&1 || true
  echo "--- storage ---" >&2
  "$adb" -s "$serial" shell df /data >&2 2>&1 || true
  echo "--- users ---" >&2
  printf 'am get-started-user-state 0: ' >&2
  "$adb" -s "$serial" shell am get-started-user-state 0 >&2 2>&1 || true
  "$adb" -s "$serial" shell dumpsys user >&2 2>&1 || true
  # The one thing no previous failure captured. If system_server is dying under
  # this gate, its death is in here and every future diagnosis starts from it
  # instead of from inference.
  echo "--- crash and system log tail ---" >&2
  "$adb" -s "$serial" logcat -d -b crash -b system -t 400 >&2 2>&1 || true
  exit 1
}

echo "API-37-gate: waiting for $serial to become ready (budget ${budget}s)"

# Narrated, because the load ceiling can legitimately hold this loop for two or
# three minutes and a gate that prints nothing for three minutes is
# indistinguishable from a gate that has hung. Only transitions are printed, so
# a guest that spends 150s waiting for its load average to decay says so once
# and then says what it is waiting for next.
last_reason="nothing was sampled"
while : ; do
  # The fatal line repeats the predicate rather than saying only "never became
  # ready". A gate whose timeout message is the same sentence regardless of
  # which condition failed makes the next reader scroll for the answer, and the
  # dump below is long.
  out_of_budget &&
    report_and_fail "the guest never reported a ready platform; last unsatisfied condition: $last_reason"
  reason=$(first_unready_reason)
  [ -z "$reason" ] && break
  if [ "$reason" != "$last_reason" ]; then
    echo "API-37-gate: waiting at $(elapsed)s: $reason"
    last_reason=$reason
  fi
  sleep "$sample_interval"
done
echo "API-37-gate: platform reported ready after $(elapsed)s"

# Unlock is waited for here, once, rather than being folded into the conditions
# above and re-read on every sample. Both placements were tried. Inside the
# sampling loop, run 32515541756 never completed four consecutive samples in the
# whole 600s budget while the timeout dump showed user 0 in RUNNING_UNLOCKED the
# entire time: the loop was measuring the probe's reliability, not the guest's
# readiness. Unlock is also monotonic in a way the binder services are not -
# a user does not spontaneously re-lock - so it wants establishing once, like
# the probe install, not asserting continuously.
while : ; do
  out_of_budget && report_and_fail "user 0 never reached RUNNING_UNLOCKED"
  [ "$(user0_state)" = unlocked ] && break
  sleep "$sample_interval"
done
echo "API-37-gate: user 0 unlocked after $(elapsed)s"

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
last_reason="nothing was sampled"
while [ "$settled" -lt "$stable_samples" ]; do
  out_of_budget &&
    report_and_fail "the guest never stayed ready for $stable_samples consecutive samples; last unsatisfied condition: $last_reason"
  sleep "$sample_interval"
  reason=$(first_unready_reason)
  if [ -z "$reason" ]; then
    settled=$((settled + 1))
  else
    # A regression here is the run-32506293772 shape: ready, installed, then a
    # service disappears. Start the count over rather than averaging over it,
    # and say which condition went, so a window that keeps resetting accuses
    # something specific instead of leaving the next run to guess.
    if [ "$settled" -ne 0 ]; then
      echo "API-37-gate: $reason at $(elapsed)s; restarting the stability window"
    fi
    last_reason=$reason
    settled=0
  fi
done

# The window above deliberately does not re-read the unlock state on every
# sample, so assert it once more here. A system_server restart mid-window is the
# one thing that could have undone it, and that restart would also have taken
# the binder services with it - which the window does watch.
#
# Asserted as "must be provably unlocked" rather than "must not be locked". The
# difference is not pedantry: user0_state answers "unknown" when its probes do
# not come back, and a locked guest whose probes are slow answers "unknown", not
# "locked" - so `= locked` let exactly the guest this gate exists to catch
# through to instrumentation, where it produced 40 failures instead of one
# named readiness failure. Requiring proof rather than absence of disproof costs
# nothing on a healthy guest, which answers "unlocked" on the first read.
if [ "$(user0_state)" != unlocked ]; then
  report_and_fail "user 0 was not provably unlocked at handover"
fi

echo "API-37-gate: guest ready and stable across $stable_samples samples after $(elapsed)s"
echo "API-37-gate: handover load $(guest_load1) on $(guest_core_count || echo '?') cores"

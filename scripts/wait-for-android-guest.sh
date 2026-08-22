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
#     `dumpsys mount` all transact into system_server and have to be answered,
#     inside a latency ceiling rather than merely eventually - and additionally
#     require the guest's runqueue to be no deeper than its cores can serve,
#     because a guest with ten runnable tasks on two cores is not ready no
#     matter what it answers.
#   * ...but read from `/proc/stat`, not `/proc/loadavg`. The first attempt at
#     that runqueue ceiling used the one-minute load average and run
#     32526074070 showed why that cannot work here: across 166 samples spanning
#     the full 900s budget the figure rose to ~30 in two minutes and then never
#     fell, sitting between 24 and 38 for the remaining thirteen, while the
#     same guest reported `2/974` - two runnable tasks - and answered every
#     binder round trip in tens of milliseconds. Linux's load average sums the
#     runqueue with uninterruptible IO wait, and on this runner's virtual disk
#     the second term is ~30 permanently. A ceiling on the sum is not a strict
#     predicate there, it is an unsatisfiable one. `procs_running` is the term
#     that means what this gate meant.
#
# So this waits for the capability and then requires it to survive. The probe
# install is deliberately performed *before* the stability window rather than
# after it: installing is the heaviest thing that happens to a young guest, it
# is what triggers dexopt, and if that is going to knock a service over then it
# has to knock it over while this script is still watching. A guest that
# installs an APK and still answers a real round trip to `package`, `activity`
# and `mount` promptly several samples later, with a runqueue its own core count
# can carry, is ready in a way none of the earlier definitions could express.
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
# 900s rather than the 600s this used to allow. The runqueue and latency
# ceilings below are conditions the old definition did not have, and a guest
# leaving a boot storm needs real time to satisfy them - run 32526074070 spent
# its first two minutes with the runqueue still climbing. The enclosing job
# allows 75 minutes and the gate script that follows this one is the long pole,
# so the extra five minutes are affordable; being unable to afford them would be
# an argument for a bigger runner, not for handing the install a guest that is
# still thrashing.
budget=${COVALENT_ANDROID_GUEST_READY_TIMEOUT:-900}
# Four samples five seconds apart. Long enough to span a system_server restart,
# which takes tens of seconds to drop and re-register its services, and short
# enough that a healthy guest pays twenty seconds for the proof.
# Six, not four. The runqueue ceiling was the gate's second line of defence
# against handing over a guest that then Watchdog-kills mid-instrumentation, and
# demoting it to an observation removes that. What replaces it is more of the
# measurement that actually predicts the failure: the three system_server round
# trips now have to hold for six consecutive samples rather than four, so the
# guest must stay answering for half again as long before the gate lets go.
stable_samples=${COVALENT_ANDROID_GUEST_STABLE_SAMPLES:-6}
sample_interval=${COVALENT_ANDROID_GUEST_SAMPLE_INTERVAL:-5}
started=$(date +%s)

# A continuous recording of the guest's own event log, started before the first
# probe and read only if this script exits 1.
#
# The failure dump this replaces asked the guest for `logcat -d -b crash -b
# system -t 400` after the fact. On this image 400 lines of the system buffer is
# about four seconds of history, which is why nine rounds of post-mortems could
# describe the guest's *state* at give-up and never its *trajectory*. Run
# 32536862682 gave up with `dumpsys user` reporting user 0 started 16 seconds
# earlier on a guest that had been up for fifteen minutes, and with zygote64,
# surfaceflinger and system_server holding adjacent late pids (12334/12340/12410)
# while vold, ueventd and tombstoned still held their original boot pids - the
# init signature of system_server dying and taking the framework with it. None of
# the four seconds of log that survived contained the death.
#
# `-b events -b crash` is the buffer pair that names the cause: am_proc_died,
# am_kill, am_crash, am_anr, am_wtf and boot_progress_* are all binary event-log
# records, and a native death lands in the crash buffer. Both are low volume -
# the chatty per-frame logging lives in `main` and `system`, which are
# deliberately not recorded here - so this is one adb pipe writing a few hundred
# kilobytes over fifteen minutes. It costs the guest nothing and it costs the
# stability window nothing: no probe waits on it, and it is spawned once, here,
# rather than sampled. logd and adbd both survive a framework restart, so the
# recording spans the very event it exists to capture.
guest_timeline=$(mktemp "${TMPDIR:-/tmp}/covalent-api37-timeline.XXXXXX")
"$adb" -s "$serial" logcat -b events -b crash -v threadtime >"$guest_timeline" 2>&1 &
guest_timeline_pid=$!
# Killed on every exit path, including the success path, so a passing run leaves
# no stray adb client attached to the device the instrumentation is about to use.
cleanup_timeline() {
  kill "$guest_timeline_pid" 2>/dev/null || true
  rm -f "$guest_timeline"
}
trap cleanup_timeline EXIT HUP INT TERM

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

# How long one binder round trip is allowed to take. This is a latency ceiling,
# not merely a liveness deadline: the question is not whether system_server
# eventually answers but whether it answers *promptly*, because an `adb install`
# is a long sequence of transactions and a guest that needs seconds for one
# needs minutes for the sequence.
#
# Three seconds is calibrated against measurements, not chosen. A healthy guest
# answers `cmd package path android` end to end - adb, shell spawn, binder, and
# back - in 317ms locally and under 100ms on the CI runner. Run 32522700913's
# paralysed guest logged `Slow dispatch took 1863ms` and lock holds of
# 1.2s/1.7s/2.0s/2.4s/3.3s, and under induced starvation locally the same probe
# took 12499ms while `service check package` still answered in 1165ms. So three
# seconds is roughly ten times healthy and below every observed pathological
# hold.
probe_deadline=${COVALENT_ANDROID_GUEST_PROBE_TIMEOUT:-3}

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

# The activity every Compose test in this suite launches, asked of the same
# service, in the same way, that androidx.test asks it.
#
# Run 32535337631 is why this exists. That run handed over at 78s with the
# platform ready, CE unlocked, all three round trips answering - and the very
# first evidence line printed at instrumentation start read `ComponentActivity :
# No activity found`, followed by three AccessibilityGateTest failures reading
# `Unable to resolve activity for: Intent { ... cmp=life.michaelwong.covalent
# .test/androidx.activity.ComponentActivity }`. That `.test` component is
# androidx.test's InstrumentationActivityInvoker falling back to the test
# package after the target package's copy failed to resolve; the fallback is a
# symptom, and the unresolvable activity is the cause.
#
# `androidx.compose.ui:ui-test-manifest` is a debugImplementation dependency, so
# ComponentActivity is declared by the *app* under test, and it becomes
# resolvable only once PackageManager has finished scanning the APK the probe
# install above just pushed. The gate already dumped this exact fact as
# post-mortem evidence while never once requiring it. Requiring it is the whole
# fix: it is not a proxy for readiness, it is the precondition the failing tests
# have, stated directly.
app_package=${COVALENT_ANDROID_APP_PACKAGE:-life.michaelwong.covalent}
launch_component=${COVALENT_ANDROID_LAUNCH_COMPONENT:-androidx.activity.ComponentActivity}

# Matched on the component line, anchored. `resolve-activity --brief` prints a
# `priority=...` line first and then the component, and prints `No activity
# found` when it cannot resolve - which an unanchored match would happily read
# past.
launch_activity_resolves() {
  bounded_shell cmd package resolve-activity --brief "$app_package/$launch_component" |
    grep -q "^$app_package/$launch_component\$"
}

# Only meaningful once the probe install has landed, so the readiness window
# before that install must not ask. Flipped to 1 immediately after it.
require_launchable=0

activity_service_answers() {
  bounded_shell cmd activity get-current-user | grep -qE '^[0-9]+$'
}

mount_service_answers() {
  bounded_shell dumpsys mount | grep -q 'CE unlocked users:'
}

# The guest's own core count, so the runqueue ceiling below is a statement about
# this guest rather than a constant that quietly stops matching. `cores` in
# ci.yml has already been changed twice - 4, then 2 - and the number of cores is
# exactly what a runqueue depth has to be read relative to: four runnable tasks
# is a busy-but-fine four-core guest and a two-core guest with twice the work it
# can serve.
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

# `procs_running` from /proc/stat, not the one-minute figure from
# /proc/loadavg, and the difference is the whole reason this predicate was
# rewritten.
#
# Linux's load average counts TASK_RUNNING *and* TASK_UNINTERRUPTIBLE, so it
# sums CPU demand and disk-IO waiting into one number. On the CI runner those
# two terms are wildly different sizes. Run 32526074070 sampled the guest 166
# times across a full 900s budget: the one-minute load started at 7.65, rose to
# ~30 within two minutes, and then sat between 24 and 38 for the entire
# remaining fifteen minutes without ever decaying - while the same dump showed
# `2/974`, only two runnable tasks out of 974, and all three binder round trips
# answering in tens of milliseconds. Roughly thirty tasks were parked in
# uninterruptible IO on the runner's virtual disk, permanently. A ceiling on the
# sum is therefore not a strict predicate on that runner, it is an unsatisfiable
# one, and an unsatisfiable predicate is a broken predicate however well
# motivated.
#
# The CPU term is the one that matters and it is separately available.
# `/proc/pressure/cpu` would be the ideal instrument but SELinux denies it to
# the shell user on this image (`/proc/pressure/io` is readable, `cpu` is not),
# whereas `/proc/stat` is readable and reports the two terms apart:
# `procs_running` is the runqueue, `procs_blocked` is the IO wait. Run
# 32522700913 - the paralysed guest this gate exists to catch - read `10/385`:
# ten runnable on two cores, five times what they can serve. The healthy guests
# measured here read 1 or 2.
guest_runnable() {
  bounded_shell grep '^procs_running' /proc/stat | awk 'NR==1 { print $2 }' |
    grep -E '^[0-9]+$' || true
}

# Kept for the failure dump and the handover line only, never as a predicate.
# It is still the right thing to *report* - it is how every other post-mortem on
# this gate described the guest - it is just not something this runner will ever
# let fall.
guest_loadavg() {
  bounded_shell cat /proc/loadavg | tr -d '\n' || true
}

# system_server's pid and its elapsed running time, as one round trip.
#
# This is the measurement that turns "the guest is unhealthy" into "the framework
# restarted at t=N", and it is deliberately one `ps` rather than a `pidof` plus a
# `/proc/uptime`: the readiness loop already makes eight round trips per sample
# and this is the ninth, so it has to be worth its place. `ps -o ETIME` answers
# both halves at once - a pid that changes between two samples proves a restart,
# and an elapsed time that resets proves it even when the pid is not visible in
# the same log line.
#
# Printed on the sample lines because that is where it is decisive. The cadence
# of the failures is the thing under investigation, and a pid recorded only in
# the post-mortem describes one instant; a pid recorded beside every sample
# describes the period.
guest_framework_pulse() {
  bounded_shell ps -A -o PID,ETIME,NAME |
    awk '$3 == "system_server" { printf "system_server pid %s up %s", $1, $2; found = 1 }
         END { if (!found) printf "system_server absent" }'
}

# surfaceflinger's and system_server's pids, read together, as the direct
# measurement of the fault this gate spent nine rounds unable to name.
#
# That fault is an abort loop. The guest's RegionSamplingThread trips
# `Assertion failed: !rcEnc->featureInfo()->hasReadColorBufferDma` inside
# GoldfishMapper::readFromHost, surfaceflinger takes SIGABRT, system_server
# follows it down and the framework restarts - about every 29 seconds, forever.
# Every readiness condition in this file is satisfiable in the gaps between those
# restarts, which is exactly how this gate could declare a guest ready and hand
# instrumentation a guest that then failed: a sample can say the guest is
# answering now, and no number of samples can say it is the same guest it was
# thirty seconds ago. Two pids can say that.
#
# Retried up to three times before reporting "absent", because one `ps` that
# missed its deadline is not evidence of a dead process, and this reading is
# allowed to fail the gate.
framework_pids() {
  attempt=1
  reading=""
  while [ "$attempt" -le 3 ]; do
    reading=$(bounded_shell ps -A -o PID,NAME |
      awk '$2 == "surfaceflinger" { sf = $1 }
           $2 == "system_server" { ss = $1 }
           END { printf "surfaceflinger=%s system_server=%s", (sf ? sf : "absent"), (ss ? ss : "absent") }')
    case "$reading" in
      *absent*) ;;
      *) printf '%s' "$reading"; return 0 ;;
    esac
    attempt=$((attempt + 1))
  done
  printf '%s' "$reading"
}

# Multiplier over the core count. Two means "the runqueue may be one task deep
# per core beyond the one running" - room for a guest that is working without
# being a guest that is drowning. Overridable, because the right number is a
# property of the runner and this file should not be edited to find out.
runnable_multiplier=${COVALENT_ANDROID_GUEST_RUNNABLE_MULTIPLIER:-2}

# Whether that ceiling *vetoes* a sample, as opposed to being reported next to
# it. Off, and run 32532615844 is why.
#
# That run sampled for 900s with the ceiling gating and reset the stability
# window 32 times. Every reset named the runqueue. The window only prints a
# reset once it has already banked a wholly good sample, so each of those 32
# lines is a record of the guest passing all three real binder round trips and
# then having that thrown away by one instantaneous reading of /proc/stat. The
# give-up dump found every round trip answering normally - `cmd package path
# android` returned package:/system/framework/framework-res.apk - with
# procs_running at 3. And because the ceiling is checked before the round trips
# and returns early, not one of the 32 resets ever established that anything
# was actually wrong with system_server.
#
# The quantity itself is the problem. `am wait-for-broadcast-idle` reports the
# queues idle at 17s, so the 7-to-74 oscillation that continues for the
# remaining 880s is not a boot storm draining, it is what this image's
# background work looks like indefinitely; two cores and three cores produced
# the same picture. A predicate a healthy guest fails at random is not a strict
# gate, it is a coin flip - and worse, a short-circuiting one that suppresses
# the measurement that would have said so.
#
# This is the third proxy in this file retired in favour of the thing it stood
# for. `service check` gave way to real binder round trips; the load average
# gave way to procs_running; procs_running now gives way to the round trips it
# was itself only ever a proxy for. It is still read, still printed on every
# sample and at handover, and still enforceable with
# COVALENT_ANDROID_GUEST_ENFORCE_RUNQUEUE=1. What it no longer does is veto a
# guest that is demonstrably answering.
enforce_runqueue=${COVALENT_ANDROID_GUEST_ENFORCE_RUNQUEUE:-0}

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
# Ordered cheapest-first, which used to be load-bearing and is now only an
# economy. While the runqueue ceiling vetoed samples, ordering it ahead of the
# round trips meant a loaded guest cost one `cat` instead of three binder
# deadlines - but it also meant 900 seconds of run 32532615844 never once
# recorded whether system_server was answering, because the cheap proxy
# returned first every time. With the ceiling demoted to an observation the
# round trips always run, so the ordering now saves nothing it was hiding.
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
  runnable=$(guest_runnable)
  if [ -z "$runnable" ]; then
    echo "the guest did not answer /proc/stat within ${probe_deadline}s"
    return
  fi
  # Both reads above stay gating: they are bounded round trips, and a guest that
  # cannot answer a `cat` of a kernel file inside the deadline has told us
  # something. It is only the comparison below that is opt-in.
  if [ "$enforce_runqueue" = 1 ]; then
    ceiling=$((cores * runnable_multiplier))
    [ "$runnable" -le "$ceiling" ] || {
      echo "guest has $runnable runnable tasks, above the ceiling $ceiling for its $cores cores"
      return
    }
  fi
  package_service_answers ||
    { echo "PackageManagerService did not answer 'cmd package path android' within ${probe_deadline}s"; return; }
  activity_service_answers ||
    { echo "ActivityManagerService did not answer 'cmd activity get-current-user' within ${probe_deadline}s"; return; }
  mount_service_answers ||
    { echo "StorageManagerService did not answer 'dumpsys mount' within ${probe_deadline}s"; return; }
  if [ "$require_launchable" = 1 ]; then
    launch_activity_resolves ||
      { echo "PackageManager cannot resolve $app_package/$launch_component, the activity every Compose test launches"; return; }
  fi
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
  # Both terms of the load average separately, because their ratio is the
  # diagnosis. Thirty blocked and two runnable is a slow disk; ten runnable on
  # two cores is the starvation that kills installs.
  echo "--- guest load ---" >&2
  printf '/proc/loadavg: ' >&2
  "$adb" -s "$serial" shell cat /proc/loadavg >&2 2>&1 || true
  # Two plain patterns rather than one alternation: `adb shell` joins its
  # arguments and the guest shell re-parses them, so an unescaped `|` becomes a
  # pipe on the device and the grep reads `^procs_blocked: inaccessible`.
  "$adb" -s "$serial" shell grep '^procs_running' /proc/stat >&2 2>&1 || true
  "$adb" -s "$serial" shell grep '^procs_blocked' /proc/stat >&2 2>&1 || true
  printf 'cores: ' >&2
  "$adb" -s "$serial" shell grep -c '^processor' /proc/cpuinfo >&2 2>&1 || true
  # Who is actually using the CPU. Every load dump before this one gave a
  # magnitude and no subject, which is how "the guest is starved" survived as a
  # diagnosis for a guest whose binder round trips were answering the whole
  # time. If a runqueue of 40 is real work, this names it; if it is one process
  # spinning, this names that instead.
  echo "--- top processes by CPU ---" >&2
  host_deadline 20 "$adb" -s "$serial" shell top -b -n 1 -m 12 -o PID,USER,%CPU,ARGS >&2 2>&1 ||
    echo "(top did not answer within 20s)" >&2
  echo "--- storage ---" >&2
  "$adb" -s "$serial" shell df /data >&2 2>&1 || true
  echo "--- users ---" >&2
  printf 'am get-started-user-state 0: ' >&2
  "$adb" -s "$serial" shell am get-started-user-state 0 >&2 2>&1 || true
  "$adb" -s "$serial" shell dumpsys user >&2 2>&1 || true
  # How old the framework is, which is the whole question when user 0 reports a
  # start time younger than the guest's uptime. vold, ueventd and tombstoned are
  # printed alongside on purpose: they are init services that a framework restart
  # does *not* touch, so if their elapsed times still match the guest's uptime
  # while zygote64, surfaceflinger and system_server do not, the kernel is fine
  # and only the Android runtime went round again. That distinction is not
  # recoverable from a pid alone.
  echo "--- framework age vs guest uptime ---" >&2
  printf '/proc/uptime: ' >&2
  "$adb" -s "$serial" shell cat /proc/uptime >&2 2>&1 || true
  # Filtered rather than dumped whole: this guest runs ~300 processes and the
  # eight names below are the ones that answer the question. The redirection is
  # on the grep, not on the adb call, because sending adb's stdout to stderr
  # ahead of the pipe leaves grep reading an empty stdin and prints the entire
  # table plus a spurious "did not answer".
  "$adb" -s "$serial" shell ps -A -o PID,PPID,ETIME,NAME 2>/dev/null |
    grep -E 'ELAPSED|zygote64|surfaceflinger|system_server|vold|ueventd|tombstoned|logd|lmkd' >&2 ||
    echo "(ps did not answer)" >&2
  # Pressure Stall Information separates the three ways a guest can be starved,
  # which the previous dumps could only guess at from free memory and a load
  # average. Measured on the local API 37 guest, `/proc/pressure/cpu` and
  # `/proc/pressure/memory` are root-only on this image and answer "Permission
  # denied" to the shell user, while `/proc/pressure/io` reads fine; they are
  # asked for anyway, separately, because the refusal is one line each, the CI
  # image may differ, and a denial recorded in the dump is better than a reader
  # wondering why the measurement is missing.
  echo "--- pressure stall information ---" >&2
  for psi in cpu memory io; do
    printf '%s: ' "$psi" >&2
    "$adb" -s "$serial" shell cat "/proc/pressure/$psi" >&2 2>&1 || true
  done
  # MemAvailable is the line that settles the memory question, and it is the one
  # the previous dumps did not have. Round 9's post-mortem read "173296K free"
  # from `top` and the RAM hypothesis was built on it, but the same output showed
  # 2.1 GB of page cache and 3.6 MB of swap used out of 3 GB - i.e. free was low
  # and available was enormous. MemAvailable states the reclaimable total
  # directly, so nobody has to reconstruct it.
  echo "--- meminfo ---" >&2
  "$adb" -s "$serial" shell cat /proc/meminfo 2>/dev/null |
    grep -E '^(MemTotal|MemFree|MemAvailable|Cached|SwapTotal|SwapFree|Dirty|Writeback)' >&2 || true
  # A native death in system_server leaves a tombstone; a Java-level kill does
  # not. An empty tombstone directory rules out half the candidate causes in one
  # line, and a populated one names the other half.
  echo "--- tombstones and ANR traces ---" >&2
  "$adb" -s "$serial" shell ls -l /data/tombstones >&2 2>&1 || true
  "$adb" -s "$serial" shell ls -l /data/anr >&2 2>&1 || true
  echo "--- kernel ring buffer tail ---" >&2
  "$adb" -s "$serial" shell dmesg 2>/dev/null | tail -n 80 >&2 ||
    echo "(dmesg is not readable by the shell user on this image)" >&2
  # The recording, not a snapshot. Everything above describes the guest at the
  # instant it gave up; this is the only part of the dump that can say when the
  # framework went down and what went with it. The filtered pass first, because a
  # single grep hit here ends nine rounds of inference: am_proc_died / am_kill
  # name a kill and its reason code, am_crash and the crash buffer name a
  # process death, am_anr names a blocked main thread, boot_progress_* mark a
  # framework coming back up, and lowmemorykiller / lmkd would name the memory
  # cause this round set out to test.
  echo "--- recorded event timeline: restart, kill and crash signatures ---" >&2
  if [ -s "$guest_timeline" ]; then
    grep -aiE 'am_proc_died|am_kill|am_crash|am_anr|am_wtf|boot_progress|am_restart|watchdog|lowmemorykiller|lmkd|Fatal signal|system_server' \
      "$guest_timeline" >&2 ||
      echo "(nothing matched in $(wc -l <"$guest_timeline") recorded event lines)" >&2
    echo "--- recorded event timeline: last 200 lines verbatim ---" >&2
    tail -n 200 "$guest_timeline" >&2 2>&1 || true
  else
    echo "(the event recording is empty; logcat -b events -b crash produced nothing)" >&2
  fi
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

# `am wait-for-broadcast-idle` is how AOSP's own harnesses wait out the boot
# broadcast storm, and it *is* supported here: on a settled API 37 guest it
# prints "All broadcast queues are idle!" and exits 0 immediately. An earlier
# revision of this block called it unbounded with both streams discarded, which
# made three very different outcomes look identical - the command succeeding,
# the command being rejected, and the command blocking for minutes on a guest
# whose queues never drain. That ambiguity is what let "unavailable on this
# image" get believed. Bound it and name which one happened.
#
# It stays non-fatal. The stability window below is the gate; this is the
# platform's own settled signal used as evidence, and on a guest that never
# settles the "did not return within Ns" line is the most direct statement of
# that fact the platform can make.
broadcast_idle_deadline=${COVALENT_ANDROID_GUEST_BROADCAST_IDLE_TIMEOUT:-300}
if broadcast_idle_out=$(host_deadline "$broadcast_idle_deadline" \
  "$adb" -s "$serial" shell am wait-for-broadcast-idle 2>&1); then
  echo "API-37-gate: broadcast queues idle after $(elapsed)s"
else
  broadcast_idle_status=$?
  if [ "$broadcast_idle_status" -eq 124 ]; then
    echo "API-37-gate: am wait-for-broadcast-idle did not return within ${broadcast_idle_deadline}s (queues still draining at $(elapsed)s); continuing"
  else
    echo "API-37-gate: am wait-for-broadcast-idle exited ${broadcast_idle_status}: $(printf '%s' "$broadcast_idle_out" | tr -d '\r' | head -n 1); continuing"
  fi
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
# The APK is on the device; from here the gate may require its activity to
# resolve, which it could not have asked before the install.
require_launchable=1

# The framework that is about to be handed over has to be one framework for the
# whole window that proves it ready. Read once here and once at handover rather
# than asserted per sample: one comparison across the window makes the same
# statement, costs one round trip instead of six, and cannot itself become a
# source of resets.
framework_at_window_start=$(framework_pids)
case "$framework_at_window_start" in
  *absent*)
    report_and_fail "surfaceflinger or system_server is not running as the stability window opens ($framework_at_window_start)"
    ;;
esac
echo "API-37-gate: framework entering the stability window: $framework_at_window_start"

settled=0
last_reason="nothing was sampled"
while [ "$settled" -lt "$stable_samples" ]; do
  out_of_budget &&
    report_and_fail "the guest never stayed ready for $stable_samples consecutive samples; last unsatisfied condition: $last_reason"
  sleep "$sample_interval"
  reason=$(first_unready_reason)
  if [ -z "$reason" ]; then
    settled=$((settled + 1))
    # Say what the runqueue was on a sample that passed. This is the number
    # that used to veto these samples, and printing it beside a proven-good
    # guest is what keeps its demotion honest rather than quiet: if handover
    # keeps succeeding at runqueue 40, that is the evidence the ceiling was
    # wrong, and if instrumentation starts dying after a high-runqueue
    # handover, that is the evidence it was not.
    echo "API-37-gate: ready sample $settled/$stable_samples at $(elapsed)s (runqueue $(guest_runnable), $(guest_framework_pulse))"
  else
    # A regression here is the run-32506293772 shape: ready, installed, then a
    # service disappears. Start the count over rather than averaging over it,
    # and say which condition went, so a window that keeps resetting accuses
    # something specific instead of leaving the next run to guess.
    if [ "$settled" -ne 0 ]; then
      echo "API-37-gate: $reason at $(elapsed)s (runqueue $(guest_runnable), $(guest_framework_pulse)); restarting the stability window"
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

# Whether the guest that passed those samples was one guest.
#
# scripts/disable-android-guest-launcher.sh has already taken the
# CompositionSamplingListener registrant off this image before this script
# started; this is what proves that it worked, and it is the only thing that can.
# A green run with the abort loop still alive is luck - the loop leaves gaps wide
# enough for six five-second samples - so "the tests passed" was never evidence
# and is not accepted as evidence here. If either pid has changed since the
# window opened, the framework restarted inside it, and this gate says so by name
# instead of passing the guest on to instrumentation to fail there.
framework_at_handover=$(framework_pids)
case "$framework_at_handover" in
  *absent*)
    report_and_fail "surfaceflinger or system_server is not running at handover ($framework_at_handover)"
    ;;
esac
if [ "$framework_at_handover" != "$framework_at_window_start" ]; then
  report_and_fail "the framework restarted during the stability window: entered as '$framework_at_window_start', left as '$framework_at_handover'"
fi

echo "API-37-gate: guest ready and stable across $stable_samples samples after $(elapsed)s"
echo "API-37-gate: framework held across the window: $framework_at_handover"
echo "API-37-gate: handover with $(guest_runnable) runnable on $(guest_core_count || echo '?') cores, loadavg $(guest_loadavg)"
# Report the ActivityManager lifecycle beside the CE key set, because run
# 32535337631 showed them disagreeing: `CE unlocked users: [0]` while
# `am get-started-user-state 0` still said BOOTING. The CE key is what
# PackageManager's direct-boot filtering consults and is what this gate waits
# on; the lifecycle is a different field and is not required here, because the
# thing that disagreement was actually costing us - ComponentActivity failing
# to resolve - is now gated on directly above. Printed so the next reader sees
# the disagreement instead of rediscovering it.
echo "API-37-gate: user 0 CE key set unlocked; ActivityManager lifecycle $(bounded_shell am get-started-user-state 0 | head -n 1)"
echo "API-37-gate: $app_package/$launch_component resolves"

#!/bin/sh
# Mutation battery for the API 37 device gate contract.
#
# The gate this exercises is the only evidence that the instrumentation suite
# ever ran on a real device, so every regression it exists to catch is
# constructed here and asserted to be rejected. A gate nobody mutation-tests
# drifts into a gate that cannot fail - which is exactly how its predecessor
# ended up demanding a hardcoded "OK (15 tests)" that no run could ever produce.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_root/scripts/android-instrumentation-result.sh"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/covalent-instrumentation-result.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT INT TERM

failures=0

reject() {
  # reject <label> <log> <suite> - the validator must refuse this shape.
  label=$1
  if validate_android_api37_result "$2" "$3" >"$work_dir/out" 2>"$work_dir/err"; then
    echo "MUTATION SURVIVED: $label was accepted" >&2
    failures=$((failures + 1))
  else
    echo "  rejected as designed: $label"
  fi
}

accept() {
  label=$1
  if validate_android_api37_result "$2" "$3" >"$work_dir/out" 2>"$work_dir/err"; then
    echo "  accepted as designed: $label"
  else
    echo "CLEAN CASE REJECTED: $label" >&2
    cat "$work_dir/err" >&2
    failures=$((failures + 1))
  fi
}

# Render an AndroidJUnitRunner `am instrument -w -r` log for a suite listing.
# `$2` is the JUnit summary total, which callers vary independently of the
# per-test records so the self-consistency check can be mutated on its own.
render_log() {
  suite_file=$1
  summary_total=$2
  total=$(grep -c '^' "$suite_file")
  current=0
  while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    class_name=${entry%%#*}
    method=${entry##*#}
    current=$((current + 1))
    for code in 1 0; do
      printf 'INSTRUMENTATION_STATUS: class=%s\n' "$class_name"
      printf 'INSTRUMENTATION_STATUS: current=%s\n' "$current"
      printf 'INSTRUMENTATION_STATUS: id=AndroidJUnitRunner\n'
      printf 'INSTRUMENTATION_STATUS: numtests=%s\n' "$total"
      printf 'INSTRUMENTATION_STATUS: stream=\n'
      printf '%s:\n' "$class_name"
      printf 'INSTRUMENTATION_STATUS: test=%s\n' "$method"
      printf 'INSTRUMENTATION_STATUS_CODE: %s\n' "$code"
    done
  done < "$suite_file"
  printf 'INSTRUMENTATION_RESULT: stream=\n'
  printf '\nTime: 12.345\n\n'
  printf 'OK (%s tests)\n\n' "$summary_total"
  printf 'INSTRUMENTATION_CODE: -1\n'
}

# ---------------------------------------------------------------------------
# 1. Derivation: the expected suite comes from source, never from a constant.
# ---------------------------------------------------------------------------
fixture="$work_dir/fixture/src/androidTest/java/life/michaelwong/covalent"
mkdir -p "$fixture/node"
cat > "$fixture/ExampleTest.kt" <<'KOTLIN'
package life.michaelwong.covalent

import org.junit.Test

class ExampleTest {
    @Test
    fun firstBehaviourHolds() {
    }

    @Test
    fun secondBehaviourHolds() {
    }
}
KOTLIN
cat > "$fixture/node/NodeExampleTest.kt" <<'KOTLIN'
package life.michaelwong.covalent.node

import org.junit.Test

class NodeExampleTest {
    @Test
    fun nodeBehaviourHolds() {
    }
}
KOTLIN

suite="$work_dir/suite.txt"
derive_android_instrumentation_suite "$work_dir/fixture/src/androidTest" > "$suite"
expected_fixture='life.michaelwong.covalent.ExampleTest#firstBehaviourHolds
life.michaelwong.covalent.ExampleTest#secondBehaviourHolds
life.michaelwong.covalent.node.NodeExampleTest#nodeBehaviourHolds'
if [ "$(cat "$suite")" != "$expected_fixture" ]; then
  echo "derivation did not produce the expected fixture suite:" >&2
  cat "$suite" >&2
  failures=$((failures + 1))
fi

if derive_android_instrumentation_suite "$work_dir/fixture/does-not-exist" >/dev/null 2>&1; then
  echo "MUTATION SURVIVED: derivation accepted a missing suite root" >&2
  failures=$((failures + 1))
fi
mkdir -p "$work_dir/empty-suite"
if derive_android_instrumentation_suite "$work_dir/empty-suite" >/dev/null 2>&1; then
  echo "MUTATION SURVIVED: derivation accepted a suite root with no sources" >&2
  failures=$((failures + 1))
fi
cp "$fixture/ExampleTest.kt" "$work_dir/ExampleTest.kt.bak"
cat > "$fixture/ExampleTest.kt" <<'KOTLIN'
package life.michaelwong.covalent

import org.junit.Ignore
import org.junit.Test

class ExampleTest {
    @Ignore("flaky")
    @Test
    fun firstBehaviourHolds() {
    }

    @Test
    fun secondBehaviourHolds() {
    }
}
KOTLIN
if derive_android_instrumentation_suite "$work_dir/fixture/src/androidTest" >/dev/null 2>&1; then
  echo "MUTATION SURVIVED: derivation accepted an @Ignore'd device test" >&2
  failures=$((failures + 1))
fi
cp "$work_dir/ExampleTest.kt.bak" "$fixture/ExampleTest.kt"

# ---------------------------------------------------------------------------
# 2. The clean case must pass, so a green gate still means something.
# ---------------------------------------------------------------------------
render_log "$suite" 3 > "$work_dir/clean.log"
accept "a run where every derived test reported OK" "$work_dir/clean.log" "$suite"

# The same clean run with adb's CRLF translation applied must still pass.
sed 's/$/\r/' "$work_dir/clean.log" > "$work_dir/crlf.log"
accept "the same run with CRLF line endings" "$work_dir/crlf.log" "$suite"

# ---------------------------------------------------------------------------
# 3. Adding a test must not break the gate; not running it must.
# ---------------------------------------------------------------------------
cat > "$fixture/node/AddedTest.kt" <<'KOTLIN'
package life.michaelwong.covalent.node

import org.junit.Test

class AddedTest {
    @Test
    fun theNewBehaviourHolds() {
    }
}
KOTLIN
grown_suite="$work_dir/grown-suite.txt"
derive_android_instrumentation_suite "$work_dir/fixture/src/androidTest" > "$grown_suite"
if [ "$(grep -c '^' "$grown_suite")" -ne 4 ]; then
  echo "derivation did not pick up a newly added test" >&2
  failures=$((failures + 1))
fi
reject "a new test added to source but absent from the device run" "$work_dir/clean.log" "$grown_suite"
render_log "$grown_suite" 4 > "$work_dir/grown.log"
accept "the same new test once it actually runs and passes" "$work_dir/grown.log" "$grown_suite"
rm "$fixture/node/AddedTest.kt"

# ---------------------------------------------------------------------------
# 4. Every way a run can be wrong.
# ---------------------------------------------------------------------------
# A test silently dropped from the device run.
grep -v 'secondBehaviourHolds' "$suite" > "$work_dir/short-suite.txt"
render_log "$work_dir/short-suite.txt" 3 > "$work_dir/dropped.log"
reject "a derived test that never ran on the device" "$work_dir/dropped.log" "$suite"

# A test that ran and failed (AndroidJUnitRunner FAILURE code -2).
awk '
  /^INSTRUMENTATION_STATUS_CODE: 0$/ && !done { print "INSTRUMENTATION_STATUS_CODE: -2"; done = 1; next }
  { print }
' "$work_dir/clean.log" > "$work_dir/failed.log"
reject "a test that reported the FAILURE status code" "$work_dir/failed.log" "$suite"

# A test that was skipped at runtime (IGNORED code -3) rather than passing.
awk '
  /^INSTRUMENTATION_STATUS_CODE: 0$/ && !done { print "INSTRUMENTATION_STATUS_CODE: -3"; done = 1; next }
  { print }
' "$work_dir/clean.log" > "$work_dir/skipped.log"
reject "a test that was skipped at runtime instead of passing" "$work_dir/skipped.log" "$suite"

# A test that only ever reported START and never concluded.
awk '
  /^INSTRUMENTATION_STATUS_CODE: 0$/ && !done { done = 1; next }
  { print }
' "$work_dir/clean.log" > "$work_dir/incomplete.log"
reject "a test that started and never concluded" "$work_dir/incomplete.log" "$suite"

# A stack trace that contains a forged OK status must not manufacture a pass.
awk '
  /^INSTRUMENTATION_STATUS_CODE: 0$/ && !done {
    print "INSTRUMENTATION_STATUS_CODE: -2"
    print "INSTRUMENTATION_STATUS_CODE: 0"
    done = 1
    next
  }
  { print }
' "$work_dir/clean.log" > "$work_dir/forged.log"
reject "a failing test whose output also contains an OK status code" "$work_dir/forged.log" "$suite"

# A stale test APK running tests that are not in this source tree.
cat "$suite" > "$work_dir/stale-suite.txt"
printf 'life.michaelwong.covalent.GhostTest#removedLastWeek\n' >> "$work_dir/stale-suite.txt"
render_log "$work_dir/stale-suite.txt" 4 > "$work_dir/stale.log"
reject "a stale test APK running tests absent from source" "$work_dir/stale.log" "$suite"

# The runner's own summary disagreeing with the per-test records.
render_log "$suite" 2 > "$work_dir/miscounted.log"
reject "a JUnit summary that disagrees with the per-test records" "$work_dir/miscounted.log" "$suite"

# The exact shape the previous gate accepted as proof: a bare summary line with
# no per-test evidence at all.
printf '%s\n' 'OK (3 tests)' 'INSTRUMENTATION_CODE: -1' > "$work_dir/bare.log"
reject "a bare summary line with no per-test status records" "$work_dir/bare.log" "$suite"

printf '%s\n' 'OK (0 tests)' 'INSTRUMENTATION_CODE: -1' > "$work_dir/zero.log"
reject "a run that executed zero tests" "$work_dir/zero.log" "$suite"

: > "$work_dir/empty.log"
reject "an empty instrumentation log" "$work_dir/empty.log" "$suite"

# The process-level result codes.
sed 's/^INSTRUMENTATION_CODE: -1$/INSTRUMENTATION_CODE: 0/' "$work_dir/clean.log" \
  > "$work_dir/wrong-code.log"
reject "a run that did not return runner success code -1" "$work_dir/wrong-code.log" "$suite"

{ printf 'FAILURES!!! Tests run: 3,  Failures: 1\n'; cat "$work_dir/clean.log"; } \
  > "$work_dir/junit-failures.log"
reject "a run whose JUnit summary announced failures" "$work_dir/junit-failures.log" "$suite"

{ printf 'INSTRUMENTATION_ABORTED: System has crashed.\n'; cat "$work_dir/clean.log"; } \
  > "$work_dir/aborted.log"
reject "a run the instrumentation aborted" "$work_dir/aborted.log" "$suite"

# ---------------------------------------------------------------------------
# 5. An unusable expectation must fail closed, never default to something.
# ---------------------------------------------------------------------------
: > "$work_dir/empty-suite.txt"
reject "an empty expected-suite file" "$work_dir/clean.log" "$work_dir/empty-suite.txt"
printf 'not-a-test-identifier\n' > "$work_dir/malformed-suite.txt"
reject "a malformed expected-suite file" "$work_dir/clean.log" "$work_dir/malformed-suite.txt"
reject "a missing expected-suite file" "$work_dir/clean.log" "$work_dir/no-such-suite.txt"
reject "a missing instrumentation log" "$work_dir/no-such.log" "$suite"

# ---------------------------------------------------------------------------
# 6. The real tree must derive a usable suite, so the device gate is not
#    quietly asserting nothing on the day it finally runs.
# ---------------------------------------------------------------------------
real_suite="$work_dir/real-suite.txt"
derive_android_instrumentation_suite "$repo_root/apps/android/app/src/androidTest" > "$real_suite"
real_count=$(grep -c '^' "$real_suite")
if [ "$real_count" -lt 1 ]; then
  echo "the repository's own instrumentation suite derived as empty" >&2
  failures=$((failures + 1))
fi
render_log "$real_suite" "$real_count" > "$work_dir/real.log"
accept "a synthetic clean run of the repository's own $real_count-test suite" \
  "$work_dir/real.log" "$real_suite"

if [ "$failures" -ne 0 ]; then
  echo "Android instrumentation result contract: $failures assertion(s) failed" >&2
  exit 1
fi
echo "Android instrumentation result contract: ok ($real_count tests derived from source)"

#!/bin/sh
# Shared fail-closed contract for the API 37 device gate.
#
# This gate used to assert a hardcoded total - `^OK \(15 tests\)$`. That constant
# was written when the suite held 15 tests and went stale the moment the 16th
# landed, so the gate could never conclude success again; it is a required check
# in ci.yml and in all four release workflows, so the whole release lane was
# blocked on a check that could not pass. Bumping the constant only re-arms the
# same trap for the next test, so do not do that.
#
# Instead, derive the expected suite from `apps/android/app/src/androidTest` and
# assert that *every derived test reported a pass on the device*, by name. That
# gives all three properties the constant could not:
#
#   * adding a test cannot break the gate - the expectation is derived, not typed;
#   * dropping or @Ignore-ing a test fails, because its name is still derived
#     from source and will be missing from (or non-passing in) the device log;
#   * a suite that shrinks unexpectedly fails, because the derived set is the
#     floor and the log must account for each of its members individually.
#
# The count assertion is retained as a self-consistency check on the log rather
# than as the contract, so `OK (0 tests)` and a truncated log still fail closed.

# Emit the expected instrumentation suite as sorted `<fqcn>#<method>` lines.
# Fails closed on anything it cannot parse, rather than emitting a short list
# that would silently weaken the gate that consumes it.
derive_android_instrumentation_suite() {
  cai_suite_root=$1
  if [ ! -d "$cai_suite_root" ]; then
    echo "Android instrumentation suite root is missing: $cai_suite_root" >&2
    return 2
  fi

  # Collect the sources into the positional parameters so the file list survives
  # paths that a word-splitting pipeline would mangle.
  set --
  for cai_suite_file in $(find "$cai_suite_root" -name '*.kt' -type f | LC_ALL=C sort); do
    set -- "$@" "$cai_suite_file"
  done
  if [ "$#" -eq 0 ]; then
    echo "Android instrumentation suite root contains no Kotlin sources: $cai_suite_root" >&2
    return 2
  fi

  cai_suite=$(awk '
    function fail(message) {
      printf("%s:%d: %s\n", FILENAME, FNR, message) > "/dev/stderr"
      failed = 1
      exit 1
    }
    FNR == 1 { package_name = ""; class_name = ""; pending = 0 }
    { sub(/\r$/, "") }
    /^[[:space:]]*package[[:space:]]+[A-Za-z0-9_.]+[[:space:]]*$/ {
      package_name = $2
      next
    }
    /^(public |internal |private |open |abstract |final |sealed |data )*class [A-Za-z0-9_]+/ {
      for (index_ = 1; index_ <= NF; index_++) {
        if ($index_ == "class") {
          class_name = $(index_ + 1)
          gsub(/[^A-Za-z0-9_].*$/, "", class_name)
          break
        }
      }
      next
    }
    /^[[:space:]]*@Ignore([[:space:]]|\(|$)/ {
      fail("@Ignore is not allowed in the API 37 device suite: a skipped test cannot prove anything, and the device gate derives its expectations from this tree")
    }
    /^[[:space:]]*@Test([[:space:]]|\(|$)/ {
      if (package_name == "") { fail("@Test found before a package declaration") }
      if (class_name == "") { fail("@Test found before a top-level class declaration") }
      pending = 1
      next
    }
    pending && /[[:space:]]fun[[:space:]]/ {
      method = $0
      sub(/^.*[[:space:]]fun[[:space:]]+/, "", method)
      sub(/[[:space:]]*\(.*$/, "", method)
      if (method !~ /^[A-Za-z0-9_]+$/) {
        fail("could not derive a plain test method name from this declaration")
      }
      printf("%s.%s#%s\n", package_name, class_name, method)
      pending = 0
      next
    }
    END {
      if (failed) { exit 1 }
      if (pending) {
        print "a trailing @Test annotation has no test method" > "/dev/stderr"
        exit 1
      }
    }
  ' "$@" | LC_ALL=C sort -u) || {
    echo "Could not derive the Android instrumentation suite from $cai_suite_root." >&2
    return 2
  }

  if [ -z "$cai_suite" ]; then
    echo "Derived an empty Android instrumentation suite from $cai_suite_root." >&2
    return 2
  fi
  printf '%s\n' "$cai_suite"
}

# Assert that the device really ran, and passed, exactly the derived suite.
# Return 1 for a rejected run, 2 for an unusable expectation.
validate_android_api37_result() {
  cai_result_log=$1
  cai_expected_suite=$2

  if [ ! -f "$cai_result_log" ]; then
    echo "Android instrumentation log is missing: $cai_result_log" >&2
    return 1
  fi
  if [ ! -s "$cai_expected_suite" ]; then
    echo "Android instrumentation expectation is missing or empty: $cai_expected_suite" >&2
    return 2
  fi
  if grep -Eqv '^[A-Za-z0-9_.]+#[A-Za-z0-9_]+$' "$cai_expected_suite"; then
    echo "Android instrumentation expectation is malformed; want '<fqcn>#<method>' per line." >&2
    return 2
  fi

  if grep -Eq '^(FAILURES!!!|INSTRUMENTATION_FAILED:|INSTRUMENTATION_ABORTED:)' "$cai_result_log"; then
    echo "Android instrumentation reported a failure." >&2
    return 1
  fi
  if ! grep -Eq '^INSTRUMENTATION_CODE: -1[[:space:]]*$' "$cai_result_log"; then
    echo "Android instrumentation did not return AndroidJUnitRunner success code -1." >&2
    return 1
  fi

  # `adb shell` can translate line endings, so normalise CR before parsing and
  # key every status block by the class/test pair the runner reported for it.
  # A test counts as passed only if it reported at least one OK status code (0)
  # and never reported any code other than OK or START (1) - so a stack trace
  # that happens to contain a status line cannot manufacture a pass.
  cai_observed=$(awk '
    { sub(/\r$/, "") }
    /^INSTRUMENTATION_STATUS: class=/ { class_name = substr($0, 31); next }
    /^INSTRUMENTATION_STATUS: test=/  { method = substr($0, 30); next }
    /^INSTRUMENTATION_STATUS_CODE: / {
      code = $2
      if (class_name != "" && method != "") {
        key = class_name "#" method
        seen[key] = 1
        if (code == 0) { ok[key] = 1 }
        else if (code != 1) { bad[key] = bad[key] " " code }
      }
      class_name = ""; method = ""
      next
    }
    END {
      for (key in seen) {
        if (key in bad) { printf("BAD %s%s\n", key, bad[key]) }
        else if (key in ok) { printf("PASS %s\n", key) }
        else { printf("INCOMPLETE %s\n", key) }
      }
    }
  ' "$cai_result_log" | LC_ALL=C sort)

  if [ -z "$cai_observed" ]; then
    echo "Android instrumentation log contains no per-test status records." >&2
    echo "Run 'am instrument -w -r' so each test reports its own class, name and status code." >&2
    return 1
  fi

  cai_rejected=0
  cai_not_passed=$(printf '%s\n' "$cai_observed" | grep -v '^PASS ' || true)
  if [ -n "$cai_not_passed" ]; then
    echo "Android instrumentation tests did not pass on the device:" >&2
    printf '%s\n' "$cai_not_passed" >&2
    cai_rejected=1
  fi

  cai_passed=$(printf '%s\n' "$cai_observed" | sed -n 's/^PASS //p' | LC_ALL=C sort -u)
  cai_wanted=$(LC_ALL=C sort -u "$cai_expected_suite")

  cai_missing=$(printf '%s\n' "$cai_wanted" | while IFS= read -r cai_want; do
    [ -n "$cai_want" ] || continue
    printf '%s\n' "$cai_passed" | grep -Fxq "$cai_want" || printf '%s\n' "$cai_want"
  done)
  if [ -n "$cai_missing" ]; then
    echo "Android instrumentation did not prove these expected tests passed:" >&2
    printf '%s\n' "$cai_missing" >&2
    cai_rejected=1
  fi

  cai_unexpected=$(printf '%s\n' "$cai_passed" | while IFS= read -r cai_got; do
    [ -n "$cai_got" ] || continue
    printf '%s\n' "$cai_wanted" | grep -Fxq "$cai_got" || printf '%s\n' "$cai_got"
  done)
  if [ -n "$cai_unexpected" ]; then
    echo "Android instrumentation ran tests that are not in this source tree:" >&2
    printf '%s\n' "$cai_unexpected" >&2
    echo "The installed test APK is stale; rebuild assembleDebugAndroidTest." >&2
    cai_rejected=1
  fi

  # Self-consistency: JUnit's own total must agree with the per-test records, so
  # a truncated log or an `OK (0 tests)` run cannot slip past the name checks.
  cai_expected_count=$(printf '%s\n' "$cai_wanted" | grep -c '^' )
  if ! grep -Eq "^OK \\(${cai_expected_count} tests\\)[[:space:]]*$" "$cai_result_log"; then
    echo "Android instrumentation did not summarise exactly ${cai_expected_count} passing tests." >&2
    grep -E '^(OK \(|Tests run|FAILURES)' "$cai_result_log" >&2 || \
      echo "The log has no JUnit summary line at all." >&2
    cai_rejected=1
  fi

  if [ "$cai_rejected" -ne 0 ]; then
    return 1
  fi
  echo "Android instrumentation proved all ${cai_expected_count} expected tests passed by name."
}

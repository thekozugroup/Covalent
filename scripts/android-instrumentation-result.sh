#!/bin/sh
# Shared fail-closed contract for the API 37 device gate.

validate_android_api37_result() {
  result_log=$1
  expected_count=${COVALENT_ANDROID_EXPECTED_TEST_COUNT:-15}
  case "$expected_count" in
    ''|*[!0-9]*)
      echo "COVALENT_ANDROID_EXPECTED_TEST_COUNT must be a positive integer." >&2
      return 2
      ;;
  esac
  if [ "$expected_count" -eq 0 ]; then
    echo "COVALENT_ANDROID_EXPECTED_TEST_COUNT must be positive." >&2
    return 2
  fi

  if grep -Eq '^(FAILURES!!!|INSTRUMENTATION_FAILED:|INSTRUMENTATION_ABORTED:)' "$result_log"; then
    echo "Android instrumentation reported a failure." >&2
    return 1
  fi
  if ! grep -Eq "^OK \\(${expected_count} tests\\)$" "$result_log"; then
    echo "Android instrumentation did not prove exactly ${expected_count} passing tests." >&2
    return 1
  fi
  if ! grep -Eq '^INSTRUMENTATION_CODE: -1$' "$result_log"; then
    echo "Android instrumentation did not return AndroidJUnitRunner success code -1." >&2
    return 1
  fi
}

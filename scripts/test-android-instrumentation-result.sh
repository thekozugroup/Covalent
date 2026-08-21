#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_root/scripts/android-instrumentation-result.sh"

result_dir=$(mktemp -d "${TMPDIR:-/tmp}/covalent-instrumentation-result.XXXXXX")
trap 'rm -rf "$result_dir"' EXIT INT TERM

printf '%s\n' 'OK (15 tests)' 'INSTRUMENTATION_CODE: -1' > "$result_dir/success.log"
validate_android_api37_result "$result_dir/success.log"

printf '%s\n' 'OK (15 tests)' 'INSTRUMENTATION_CODE: 0' > "$result_dir/wrong-code.log"
if validate_android_api37_result "$result_dir/wrong-code.log"; then
  echo "accepted an invalid instrumentation code" >&2
  exit 1
fi

printf '%s\n' 'FAILURES!!! Tests run: 15,  Failures: 1' 'INSTRUMENTATION_CODE: -1' > "$result_dir/failure.log"
if validate_android_api37_result "$result_dir/failure.log"; then
  echo "accepted a failed instrumentation run" >&2
  exit 1
fi

printf '%s\n' 'OK (14 tests)' 'INSTRUMENTATION_CODE: -1' > "$result_dir/wrong-count.log"
if validate_android_api37_result "$result_dir/wrong-count.log"; then
  echo "accepted an incomplete instrumentation run" >&2
  exit 1
fi

COVALENT_ANDROID_EXPECTED_TEST_COUNT=14 validate_android_api37_result "$result_dir/wrong-count.log"

echo "Android instrumentation result contract: ok"

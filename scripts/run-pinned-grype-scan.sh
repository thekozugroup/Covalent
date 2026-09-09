#!/bin/sh
# Download and run one exact Grype release against one immutable local image.
set -eu

GRYPE_VERSION=0.117.0
GRYPE_LINUX_AMD64_SHA256=38525dab1e06f162ebaa02f94d82d1f807076b011a44180cf2777edf1a7b9c26
GRYPE_LINUX_ARM64_SHA256=935f628bdf9331ffdd946931ea5fdb50045d3970ba52670cbeb44a88f127291b
MAX_ARCHIVE_BYTES=67108864
MAX_ARCHIVE_CONTENT_BYTES=134217728
# Grype's complete v6 database is currently about 1.6 GB uncompressed. Keep a
# finite allowance for that database plus the pinned scanner and update files.
# Source: https://github.com/anchore/grype/issues/3245
MAX_PRIVATE_BYTES=2147483648
SCAN_TIMEOUT=12m

fail() {
  echo "pinned Grype scan: $*" >&2
  exit 1
}

case "${1-}" in
  sha256:[0-9a-f][0-9a-f]*) image_id=$1 ;;
  *) fail "IMAGE_ID must be an exact sha256 digest" ;;
esac
if [ "${#image_id}" -ne 71 ]; then
  fail "IMAGE_ID must be an exact sha256 digest"
fi
case "${image_id#sha256:}" in *[!0-9a-f]*) fail "IMAGE_ID must be an exact sha256 digest" ;; esac

report=${2-}
[ -n "$report" ] || fail "usage: $0 IMAGE_ID REPORT"
[ "$#" -eq 2 ] || fail "usage: $0 IMAGE_ID REPORT"
report_parent=$(dirname -- "$report")
[ -d "$report_parent" ] || fail "report parent directory does not exist"
[ ! -e "$report" ] && [ ! -L "$report" ] || fail "report path already exists"

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
private_parent=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
[ -d "$private_parent" ] || fail "private temporary parent is unavailable"
work=$(mktemp -d "$private_parent/covalent-grype.XXXXXX") || fail "could not create private temporary directory"
chmod 700 "$work"
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if ! python3 - "$work" <<'PY'
import shutil
import sys
shutil.rmtree(sys.argv[1])
PY
  then
    echo "pinned Grype scan: could not remove the private scanner directory" >&2
    [ "$status" -ne 0 ] || status=1
  fi
  exit "$status"
}
trap cleanup EXIT HUP INT TERM
mkdir -m 700 "$work/cache" "$work/bin"

scanner=$work/bin/grype
if [ -n "${COVALENT_GRYPE_TEST_BINARY-}" ]; then
  [ "${COVALENT_GRYPE_TESTING-}" = 1 ] || fail "test scanner requires the explicit test guard"
  [ "${CI-}" != true ] && [ "${GITHUB_ACTIONS-}" != true ] \
    || fail "test scanner is forbidden in CI"
  [ -f "$COVALENT_GRYPE_TEST_BINARY" ] && [ -x "$COVALENT_GRYPE_TEST_BINARY" ] \
    || fail "test scanner is not an executable regular file"
  cp "$COVALENT_GRYPE_TEST_BINARY" "$scanner"
  chmod 700 "$scanner"
  expected_platform=linux/amd64
else
  [ "$(uname -s)" = Linux ] || fail "Grype release binary is supported only on Linux"
  case "$(uname -m)" in
    x86_64)
      release_arch=amd64
      expected_sha=$GRYPE_LINUX_AMD64_SHA256
      expected_platform=linux/amd64
      ;;
    aarch64|arm64)
      release_arch=arm64
      expected_sha=$GRYPE_LINUX_ARM64_SHA256
      expected_platform=linux/arm64
      ;;
    *) fail "unsupported Linux architecture: $(uname -m)" ;;
  esac
  archive=$work/grype.tar.gz
  url="https://github.com/anchore/grype/releases/download/v${GRYPE_VERSION}/grype_${GRYPE_VERSION}_linux_${release_arch}.tar.gz"
  curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
    --retry 3 --retry-all-errors --connect-timeout 20 --max-time 180 \
    --max-filesize "$MAX_ARCHIVE_BYTES" --output "$archive" "$url"
  archive_bytes=$(wc -c <"$archive" | tr -d ' ')
  [ "$archive_bytes" -le "$MAX_ARCHIVE_BYTES" ] || fail "Grype release archive exceeds the byte bound"
  actual_sha=$(sha256sum "$archive" | awk '{print $1}')
  [ "$actual_sha" = "$expected_sha" ] || fail "Grype release archive checksum mismatch"
  python3 - "$archive" "$scanner" "$MAX_ARCHIVE_CONTENT_BYTES" <<'PY'
import os
import shutil
import stat
import sys
import tarfile

archive, output, maximum_text = sys.argv[1:]
maximum = int(maximum_text)
expected = {"CHANGELOG.md", "LICENSE", "README.md", "grype"}
with tarfile.open(archive, "r:gz") as bundle:
    members = bundle.getmembers()
    if len(members) != len(expected) or {member.name for member in members} != expected:
        raise SystemExit("unexpected Grype archive contents")
    if any(not member.isfile() for member in members):
        raise SystemExit("Grype archive contains a non-regular member")
    if sum(member.size for member in members) > maximum:
        raise SystemExit("Grype archive content exceeds the bound")
    member = bundle.getmember("grype")
    source = bundle.extractfile(member)
    if source is None:
        raise SystemExit("Grype binary is unavailable")
    descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)
    with source, os.fdopen(descriptor, "wb") as destination:
        shutil.copyfileobj(source, destination, length=1024 * 1024)
PY
fi

version_output=$("$scanner" version 2>&1) || fail "could not execute the pinned Grype binary"
printf '%s\n' "$version_output" | grep -Eq '^Version:[[:space:]]+0\.117\.0[[:space:]]*$' \
  || fail "Grype binary version is not 0.117.0"
printf '%s\n' "$version_output" | grep -Eq "^Platform:[[:space:]]+$expected_platform[[:space:]]*$" \
  || fail "Grype binary architecture does not match the selected archive"

config=$work/grype.yaml
cat >"$config" <<EOF
check-for-app-update: false
only-fixed: false
ignore: []
vex-documents: []
match-upstream-kernel-headers: true
db:
  cache-dir: $work/cache/db
  auto-update: true
  validate-by-hash-on-start: true
  validate-age: true
  max-allowed-built-age: 120h0m0s
  require-update-check: true
  update-available-timeout: 30s
  update-download-timeout: 5m0s
EOF

set +e
unset GRYPE_CONFIG GRYPE_IGNORE GRYPE_VEX_ADD GRYPE_VEX_DOCUMENTS
XDG_CACHE_HOME=$work/cache \
GRYPE_CHECK_FOR_APP_UPDATE=false \
GRYPE_DB_CACHE_DIR=$work/cache/db \
GRYPE_DB_AUTO_UPDATE=true \
GRYPE_DB_VALIDATE_BY_HASH_ON_START=true \
GRYPE_DB_VALIDATE_AGE=true \
GRYPE_DB_MAX_ALLOWED_BUILT_AGE=120h0m0s \
GRYPE_DB_REQUIRE_UPDATE_CHECK=true \
GRYPE_ONLY_FIXED=false \
GRYPE_MATCH_UPSTREAM_KERNEL_HEADERS=true \
timeout "$SCAN_TIMEOUT" "$scanner" --config "$config" --fail-on high \
  --only-fixed=false --output json --file "$report" "docker:$image_id"
scan_status=$?
set -e

private_kib=
if private_usage=$(du -sk "$work" 2>/dev/null); then
  private_kib=$(printf '%s\n' "$private_usage" | awk 'NR == 1 { print $1 }')
  case "$private_kib" in *[!0-9]*|'') private_kib= ;; esac
fi
failure_status=0

# A scanner policy failure can still leave a complete, useful report. Validate
# and summarize it before evaluating independent scanner-resource failures so
# CI logs retain bounded vulnerability diagnostics from the exact image.
if [ -f "$report" ] && [ ! -L "$report" ]; then
  if ! python3 -B "$repo_root/scripts/verify-grype-report.py" "$report" "$image_id"; then
    echo "pinned Grype scan: Grype report validation failed" >&2
    failure_status=1
  fi
else
  echo "pinned Grype scan: Grype did not produce a regular JSON report" >&2
  failure_status=1
fi

if [ -z "$private_kib" ]; then
  echo "pinned Grype scan: could not measure private Grype data" >&2
  failure_status=1
else
  private_bytes=$((private_kib * 1024))
  if [ "$private_bytes" -gt "$MAX_PRIVATE_BYTES" ]; then
    echo "pinned Grype scan: private Grype data exceeded the disk bound ($private_bytes > $MAX_PRIVATE_BYTES bytes)" >&2
    failure_status=1
  fi
fi

case "$scan_status" in
  0) ;;
  2)
    echo "pinned Grype scan: high or critical vulnerability found" >&2
    [ "$failure_status" -ne 0 ] || failure_status=2
    ;;
  124)
    echo "pinned Grype scan: Grype scan exceeded the bounded deadline" >&2
    failure_status=1
    ;;
  *)
    echo "pinned Grype scan: Grype scan failed with status $scan_status" >&2
    failure_status=1
    ;;
esac

exit "$failure_status"

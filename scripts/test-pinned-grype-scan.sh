#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/covalent-grype-fixture.XXXXXX")
cleanup() {
  python3 - "$fixture" <<'PY'
import shutil
import sys
shutil.rmtree(sys.argv[1], ignore_errors=True)
PY
}
trap cleanup EXIT HUP INT TERM

mock=$fixture/mock-grype
cat >"$mock" <<'SH'
#!/bin/sh
set -eu
if [ "${1-}" = version ]; then
  printf '%s\n' 'Application: grype' 'Version: 0.117.0' 'Platform: linux/amd64'
  exit 0
fi

fail_on=false
only_fixed=false
config=
report=
target=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --config) config=$2; shift 2 ;;
    --fail-on) [ "$2" = high ] || exit 70; fail_on=true; shift 2 ;;
    --only-fixed=false) only_fixed=true; shift ;;
    --output) [ "$2" = json ] || exit 71; shift 2 ;;
    --file) report=$2; shift 2 ;;
    docker:sha256:*) target=$1; shift ;;
    *) exit 72 ;;
  esac
done
[ "$fail_on" = true ] && [ "$only_fixed" = true ] && [ -f "$config" ] || exit 73
grep -q '^ignore: \[\]$' "$config" || exit 74
grep -q '^vex-documents: \[\]$' "$config" || exit 75
grep -q '^  require-update-check: true$' "$config" || exit 76
grep -q '^match-upstream-kernel-headers: true$' "$config" || exit 77

case "${MOCK_MODE:-success}" in
  missing) exit 17 ;;
esac
image_id=${target#docker:}
reported_id=$image_id
[ "${MOCK_MODE:-success}" != wrong-image ] || reported_id=sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
ignored='[]'
[ "${MOCK_MODE:-success}" != ignored ] || ignored='[{"reason":"fixture suppression"}]'
matches='[]'
[ "${MOCK_MODE:-success}" != finding ] || matches='[{"vulnerability":{"id":"CVE-2099-0001","severity":"Critical","fix":{"versions":[]}},"artifact":{"name":"fixture-package","version":"1.0","type":"apk"}}]'
cat >"$report" <<EOF
{"matches":$matches,"ignoredMatches":$ignored,"source":{"type":"image","target":{"userInput":"$target","imageID":"$reported_id"}},"descriptor":{"name":"grype","version":"0.117.0","db":{"built":"fixture"}}}
EOF
[ "${MOCK_MODE:-success}" != finding ] || exit 2
exit 0
SH
chmod 700 "$mock"
mkdir "$fixture/bin"
cat >"$fixture/bin/timeout" <<'SH'
#!/bin/sh
shift
exec "$@"
SH
chmod 700 "$fixture/bin/timeout"

image=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
run_scan() {
  mode=$1
  report=$2
  set +e
  PATH="$fixture/bin:$PATH" CI= GITHUB_ACTIONS= COVALENT_GRYPE_TESTING=1 \
    COVALENT_GRYPE_TEST_BINARY=$mock MOCK_MODE=$mode \
    RUNNER_TEMP=$fixture "$repo_root/scripts/run-pinned-grype-scan.sh" "$image" "$report"
  status=$?
  set -e
  return "$status"
}

run_scan success "$fixture/success.json"
[ -f "$fixture/success.json" ]
if find "$fixture" -maxdepth 1 -type d -name 'covalent-grype.*' | grep -q .; then
  echo "private scanner directory was not cleaned" >&2
  exit 1
fi

if PATH="$fixture/bin:$PATH" CI=true GITHUB_ACTIONS=true COVALENT_GRYPE_TESTING=1 \
  COVALENT_GRYPE_TEST_BINARY=$mock RUNNER_TEMP=$fixture \
  "$repo_root/scripts/run-pinned-grype-scan.sh" "$image" "$fixture/forbidden.json" \
  2>"$fixture/forbidden.err"; then
  echo "test scanner was accepted in CI" >&2
  exit 1
fi
grep -q 'test scanner is forbidden in CI' "$fixture/forbidden.err"
[ ! -e "$fixture/forbidden.json" ]

if run_scan finding "$fixture/finding.json" >"$fixture/finding.log"; then
  echo "high-severity fixture did not fail" >&2
  exit 1
else
  status=$?
  [ "$status" -eq 2 ] || exit 1
fi
[ -f "$fixture/finding.json" ]
grep -q 'severities=Critical:1' "$fixture/finding.log"
grep -q 'id=CVE-2099-0001 package=fixture-package version=1.0 type=apk fixed=none' "$fixture/finding.log"

if run_scan wrong-image "$fixture/wrong-image.json"; then
  echo "wrong image binding was accepted" >&2
  exit 1
fi
if run_scan ignored "$fixture/ignored.json"; then
  echo "suppressed finding was accepted" >&2
  exit 1
fi
if run_scan missing "$fixture/missing.json"; then
  echo "missing report was accepted" >&2
  exit 1
fi

# The release gate validates both reports even when the first scan failed.
cp "$fixture/success.json" "$fixture/amd64.json"
cp "$fixture/success.json" "$fixture/arm64.json"
if "$repo_root/scripts/finalize-container-vulnerability-scans.sh" \
  failure "$image" "$fixture/amd64.json" success "$image" "$fixture/arm64.json" \
  2>"$fixture/finalize.err"; then
  echo "combined gate accepted a failed architecture" >&2
  exit 1
fi
grep -q 'linux/amd64 vulnerability scan did not succeed' "$fixture/finalize.err"
python3 - "$fixture/arm64.json" <<'PY'
import json
import sys
path = sys.argv[1]
data = json.load(open(path, encoding="utf-8"))
data["source"]["target"]["imageID"] = "sha256:" + "a" * 64
open(path, "w", encoding="utf-8").write(json.dumps(data))
PY
if "$repo_root/scripts/finalize-container-vulnerability-scans.sh" \
  success "$image" "$fixture/amd64.json" success "$image" "$fixture/arm64.json" \
  2>"$fixture/missing.err"; then
  echo "combined gate accepted invalid arm64 evidence" >&2
  exit 1
fi
grep -q 'linux/arm64 vulnerability report is missing or invalid' "$fixture/missing.err"
if "$repo_root/scripts/finalize-container-vulnerability-scans.sh" \
  success "$image" "$fixture/amd64.json" success "$image" "$fixture/absent.json" \
  2>"$fixture/absent.err"; then
  echo "combined gate accepted a missing arm64 report" >&2
  exit 1
fi
grep -q 'linux/arm64 vulnerability report is missing or invalid' "$fixture/absent.err"

python3 - "$fixture/success.json" "$fixture/summary.json" <<'PY'
import json
import sys
source, destination = sys.argv[1:]
data = json.load(open(source, encoding="utf-8"))
data["matches"] = [
    {
        "vulnerability": {"id": "CVE-2099-" + str(index).zfill(4) + "\nforged", "severity": "High", "fix": {"versions": []}},
        "artifact": {"name": "x" * 200, "version": "1", "type": "apk"},
    }
    for index in range(32)
]
open(destination, "w", encoding="utf-8").write(json.dumps(data))
PY
"$repo_root/scripts/verify-grype-report.py" "$fixture/summary.json" "$image" >"$fixture/summary.log"
[ "$(wc -l <"$fixture/summary.log" | tr -d ' ')" -eq 32 ]
grep -q 'Grype high/critical: omitted=2' "$fixture/summary.log"
if grep -q '^forged' "$fixture/summary.log"; then
  echo "report field injected a log line" >&2
  exit 1
fi

if find "$fixture" -maxdepth 1 -type d -name 'covalent-grype.*' | grep -q .; then
  echo "private scanner directory was retained after a failure" >&2
  exit 1
fi

printf '%s\n' 'Pinned Grype scan fixtures: ok'

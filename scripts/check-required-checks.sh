#!/bin/sh
# Assert that every unconditional CI job is actually required by every release
# workflow.
#
# Each release workflow re-lists the checks it demands be green on the exact
# release commit. Those lists are hand-maintained, they read as exhaustive, and
# they were not: "iOS Tier 2" was absent from all four, covered only by accident
# through the aggregate job's `needs:`. The failure mode is silent in the worst
# possible direction - a lane that is not listed is a lane a release can ship
# without, and nothing anywhere says so. So derive the list from ci.yml and
# fail closed on any lane a release workflow forgot.
#
# This is a text check on workflow text, which is the artifact itself rather
# than a proxy for it: the required-check names are exactly these strings, and
# GitHub matches them by string.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
workflows="$repo_root/.github/workflows"
ci="$workflows/ci.yml"

test -f "$ci" || { echo "missing $ci" >&2; exit 1; }

# Job display names from ci.yml, minus jobs that carry a job-level `if:` and so
# are legitimately absent on some events (dependency-review is pull-request only).
required_names=$(awk '
  /^  [A-Za-z0-9_-]+:[[:space:]]*$/ {
    if (job_name != "" && !conditional) { print job_name }
    job_name = ""; conditional = 0; in_job = 1; next
  }
  in_job && /^    name:[[:space:]]/ {
    line = $0
    sub(/^    name:[[:space:]]*/, "", line)
    gsub(/^["'\'']|["'\'']$/, "", line)
    job_name = line
    next
  }
  in_job && /^    if:[[:space:]]/ { conditional = 1; next }
  /^[A-Za-z]/ {
    if (job_name != "" && !conditional) { print job_name }
    job_name = ""; conditional = 0; in_job = 0
  }
  END { if (job_name != "" && !conditional) { print job_name } }
' "$ci")

if [ -z "$required_names" ]; then
  echo "Could not read any job names out of $ci; refusing to pass vacuously." >&2
  exit 1
fi

name_count=$(printf '%s\n' "$required_names" | grep -c '^')
if [ "$name_count" -lt 5 ]; then
  echo "Only $name_count CI job names were parsed from $ci; that is too few to be real." >&2
  printf '%s\n' "$required_names" >&2
  exit 1
fi

rejected=0
checked=0
for workflow in \
  "$workflows/android-release.yml" \
  "$workflows/apple-release.yml" \
  "$workflows/apple-unsigned-release.yml" \
  "$workflows/container-supply-chain.yml"
do
  test -f "$workflow" || { echo "missing $workflow" >&2; rejected=1; continue; }
  required_line=$(grep -n 'for required in ' "$workflow" | head -n 1 | cut -d: -f2-)
  if [ -z "$required_line" ]; then
    echo "$workflow has no 'for required in' list of exact-commit checks." >&2
    rejected=1
    continue
  fi
  checked=$((checked + 1))
  printf '%s\n' "$required_line" | grep -q '"Release candidate software gates"' || {
    echo "$workflow does not require the aggregate release-candidate gate." >&2
    rejected=1
  }
  printf '%s\n' "$required_names" | while IFS= read -r wanted; do
    [ -n "$wanted" ] || continue
    printf '%s\n' "$required_line" | grep -Fq "\"$wanted\"" || \
      printf '%s\t%s\n' "$workflow" "$wanted"
  done > "${TMPDIR:-/tmp}/covalent-required-missing.$$"
  if [ -s "${TMPDIR:-/tmp}/covalent-required-missing.$$" ]; then
    echo "$workflow does not require these CI lanes:" >&2
    sed 's/^/  /' "${TMPDIR:-/tmp}/covalent-required-missing.$$" >&2
    rejected=1
  fi
  rm -f "${TMPDIR:-/tmp}/covalent-required-missing.$$"
done

if [ "$checked" -eq 0 ]; then
  echo "No release workflow was inspected; this gate proved nothing." >&2
  exit 1
fi
if [ "$rejected" -ne 0 ]; then
  echo "Release workflows must require every unconditional CI lane by name." >&2
  exit 1
fi

echo "Required-check contract: $checked release workflows each require all $name_count CI lanes."

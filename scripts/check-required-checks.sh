#!/bin/sh
# Assert that every unconditional CI job is actually required by every release
# workflow, except for lanes this file explicitly and visibly exempts.
#
# Each release workflow re-lists the checks it demands be green on the exact
# release commit. Those lists are hand-maintained, they read as exhaustive, and
# they were not: "iOS Tier 2" was absent from all four, covered only by accident
# through the aggregate job's `needs:`. The failure mode is silent in the worst
# possible direction - a lane that is not listed is a lane a release can ship
# without, and nothing anywhere says so. So derive the list from ci.yml and
# fail closed on any lane a release workflow forgot.
#
# There is now one lane that deliberately does not gate a release. That is a
# different statement from "a lane was forgotten", and the difference has to be
# written down somewhere a reader can find it, which is what $not_required_names
# below is for. It is an allowlist of one, and it is not an escape hatch: every
# entry is cross-checked three ways so that adding a name here cannot hide a
# lane that was actually dropped. See the comment on that variable.
#
# This is a text check on workflow text, which is the artifact itself rather
# than a proxy for it: the required-check names are exactly these strings, and
# GitHub matches them by string.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
workflows="$repo_root/.github/workflows"
ci="$workflows/ci.yml"

test -f "$ci" || { echo "missing $ci" >&2; exit 1; }

tmp="${TMPDIR:-/tmp}/covalent-required-checks.$$"
mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT INT TERM

# `job key<TAB>job display name` for every ci.yml job, minus jobs that carry a
# job-level `if:` and so are legitimately absent on some events
# (dependency-review is pull-request only, release-candidate-software is
# `if: always()`). Both halves are needed: release workflows match lanes by
# display name, and the aggregate job's `needs:` matches them by key.
job_table=$(awk '
  /^  [A-Za-z0-9_-]+:[[:space:]]*$/ {
    if (job_key != "" && job_name != "" && !conditional) { print job_key "\t" job_name }
    job_key = $1; sub(/:$/, "", job_key)
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
    if (job_key != "" && job_name != "" && !conditional) { print job_key "\t" job_name }
    job_key = ""; job_name = ""; conditional = 0; in_job = 0
  }
  END { if (job_key != "" && job_name != "" && !conditional) { print job_key "\t" job_name } }
' "$ci")

all_names=$(printf '%s\n' "$job_table" | cut -f2)

# Lanes that run on every commit but deliberately do NOT gate a release.
#
# Supported platforms are Unraid, macOS, and Android. iOS is out of scope for
# now, so a red iOS lane must not be able to hold a release hostage. The job
# itself stays in ci.yml and keeps running - it is green and costs nothing - we
# simply stop treating it as a release blocker.
#
# Adding a name here cannot be used to quietly drop a lane, because each entry
# is verified three ways below:
#   1. it must still exist as an unconditional job in ci.yml, so this list
#      cannot outlive the job it exempts or cover a typo;
#   2. its job key must be absent from the aggregate gate's `needs:`, because
#      "Release candidate software gates" IS required and would otherwise
#      re-impose the lane transitively, making the exemption a lie;
#   3. its display name must be absent from every release workflow's required
#      list, so this file and the workflows cannot disagree about what gates a
#      release.
# Every lane NOT named here is still required by name in all release workflows.
not_required_names='iOS Tier 2'

printf '%s\n' "$not_required_names" > "$tmp/exempt"
printf '%s\n' "$all_names" > "$tmp/all"
required_names=$(grep -Fxv -f "$tmp/exempt" "$tmp/all" || true)

# 1. Every exempted name must still be a real unconditional ci.yml job.
while IFS= read -r exempt; do
  [ -n "$exempt" ] || continue
  grep -Fxq "$exempt" "$tmp/all" || {
    echo "\"$exempt\" is listed as deliberately-not-required but is not an unconditional job in $ci." >&2
    echo "Either the lane was dropped (restore it) or the name is stale (remove it from not_required_names)." >&2
    exit 1
  }
done < "$tmp/exempt"

# 2. An exempted lane must not sneak back in through the required aggregate job.
aggregate_needs=$(awk '
  /^  release-candidate-software:[[:space:]]*$/ { in_agg = 1; next }
  in_agg && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { in_agg = 0 }
  in_agg && /^    needs:[[:space:]]*$/ { in_needs = 1; next }
  in_needs && /^      -[[:space:]]/ { line = $0; sub(/^      -[[:space:]]*/, "", line); print line; next }
  in_needs && /^    [A-Za-z]/ { in_needs = 0 }
' "$ci")

if [ -z "$aggregate_needs" ]; then
  echo "Could not read the release-candidate-software \`needs:\` list out of $ci; refusing to pass vacuously." >&2
  exit 1
fi
printf '%s\n' "$aggregate_needs" > "$tmp/agg"

while IFS= read -r exempt; do
  [ -n "$exempt" ] || continue
  exempt_key=$(printf '%s\n' "$job_table" | awk -F'\t' -v n="$exempt" '$2 == n { print $1 }')
  if [ -n "$exempt_key" ] && grep -Fxq "$exempt_key" "$tmp/agg"; then
    echo "\"$exempt\" is listed as deliberately-not-required, but job \`$exempt_key\` is still in the" >&2
    echo "release-candidate-software \`needs:\` list, and that aggregate gate IS required by every" >&2
    echo "release workflow. The exemption would be a lie: a red $exempt still blocks a release." >&2
    exit 1
  fi
  if awk '/^  release-candidate-software:/ { a = 1 } a' "$ci" | grep -Eq "^[[:space:]]+$(printf '%s' "$exempt" | sed 's/[.[\*^$/]/\\&/g')=" ; then
    echo "\"$exempt\" is still asserted in the aggregate gate's GATE_RESULTS block in $ci." >&2
    exit 1
  fi
done < "$tmp/exempt"

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
  "$workflows/container-supply-chain.yml" \
  "$workflows/cli-release.yml"
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
  # 3. A lane this file calls deliberately-not-required must not be required
  #    here, or the two files disagree about what actually gates a release.
  while IFS= read -r exempt; do
    [ -n "$exempt" ] || continue
    if printf '%s\n' "$required_line" | grep -Fq "\"$exempt\""; then
      echo "$workflow requires \"$exempt\", which scripts/check-required-checks.sh lists as" >&2
      echo "deliberately not required. Remove it from the workflow, or remove it from" >&2
      echo "not_required_names so it is enforced everywhere." >&2
      rejected=1
    fi
  done < "$tmp/exempt"
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

exempt_count=$(grep -c '^..*$' "$tmp/exempt" || true)
echo "Required-check contract: $checked release workflows each require all $name_count CI lanes."
echo "Deliberately not required ($exempt_count, verified to run but not to gate): $(paste -sd, - < "$tmp/exempt")"

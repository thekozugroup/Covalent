#!/bin/sh
# Require every unconditional CI job and the aggregate gate on the release commit.
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
# `if: always()`). Release workflows match lanes by display name.
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

required_names=$(printf '%s\n' "$job_table" | cut -f2)

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
  printf '%s\n' "$required_names" | while IFS= read -r wanted; do
    [ -n "$wanted" ] || continue
    printf '%s\n' "$required_line" | grep -Fq "\"$wanted\"" || \
      printf '%s\t%s\n' "$workflow" "$wanted"
  done > "$tmp/missing"
  if [ -s "$tmp/missing" ]; then
    echo "$workflow does not require these CI lanes:" >&2
    sed 's/^/  /' "$tmp/missing" >&2
    rejected=1
  fi
  rm -f "$tmp/missing"
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

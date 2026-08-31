#!/bin/sh
set -eu
set -f

repo_root=$(CDPATH='' cd -P -- "$(dirname -- "$0")/.." && pwd -P)
validator=$repo_root/scripts/validate-setup-paths.sh
fixture_parent=${TMPDIR:-/tmp}
fixture_parent=${fixture_parent%/}
fixture_created=$(mktemp -d "$fixture_parent/covalent-setup-paths.XXXXXX")
fixture_dir=$(CDPATH='' cd -P -- "$fixture_created" && pwd -P)

cleanup() {
  if [ -n "${fixture_dir:-}" ] && [ "$fixture_dir" != / ] && [ -d "$fixture_dir" ]; then
    chmod -R u+rwx "$fixture_dir" 2>/dev/null || true
    rm -rf -- "$fixture_dir"
  fi
}
trap cleanup EXIT HUP INT TERM

last_output=$fixture_dir/last-output

expect_pass() {
  tsp_name=$1
  shift
  if ! "$@" >"$last_output" 2>&1; then
    printf '%s\n' "FAIL: $tsp_name should pass" >&2
    sed -n '1,120p' "$last_output" >&2
    exit 1
  fi
  grep -Fq 'Read-only validation complete; nothing was created or changed.' "$last_output" || {
    printf '%s\n' "FAIL: $tsp_name omitted read-only success proof" >&2
    exit 1
  }
}

expect_fail() {
  tsp_name=$1
  tsp_expected=$2
  shift 2
  if "$@" >"$last_output" 2>&1; then
    printf '%s\n' "FAIL: $tsp_name should fail" >&2
    exit 1
  fi
  if ! grep -Fq "$tsp_expected" "$last_output"; then
    printf '%s\n' "FAIL: $tsp_name missing expected error: $tsp_expected" >&2
    sed -n '1,120p' "$last_output" >&2
    exit 1
  fi
}

invoke() {
  tsp_config=$1
  tsp_data=$2
  tsp_restore=$3
  tsp_kek=$4
  shift 4

  tsp_sources=
  for tsp_source in "$@"; do
    if [ -n "$tsp_sources" ]; then
      tsp_sources="$tsp_sources
$tsp_source"
    else
      tsp_sources=$tsp_source
    fi
  done
  set -- \
    --config "$tsp_config" \
    --data "$tsp_data"
  tsp_saved_ifs=$IFS
  IFS='
'
  # shellcheck disable=SC2086
  for tsp_source in $tsp_sources; do
    set -- "$@" --source "$tsp_source"
  done
  IFS=$tsp_saved_ifs
  set -- "$@" --restore "$tsp_restore" --kek "$tsp_kek"
  "$validator" "$@"
}

safe_root=$fixture_dir/'safe setup'
safe_config=$safe_root/'config state'
safe_data=$safe_root/'data state'
safe_restore=$safe_root/'restore target'
safe_secrets=$safe_root/'secrets state'
safe_kek=$safe_secrets/'owner key'
safe_source_one=$safe_root/'source one'
safe_source_two=$safe_root/'source two'
mkdir -p \
  "$safe_config" "$safe_data" "$safe_restore" "$safe_secrets" \
  "$safe_source_one" "$safe_source_two"
touch "$safe_kek" "$safe_source_one/keep me" "$safe_restore/keep me too"
chmod 600 "$safe_kek"

expect_pass 'safe sibling paths with spaces and multiple sources' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_kek" \
  "$safe_source_one" "$safe_source_two"
test -f "$safe_source_one/keep me"
test -f "$safe_restore/keep me too"
test "$(find "$safe_root" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" = 6

# Equal, new-child-of-existing, and new-ancestor-of-existing exercise every
# branch in the symmetric overlap check.
expect_fail 'equal config and data paths' 'Setup paths overlap' \
  invoke "$safe_config" "$safe_config" "$safe_restore" "$safe_kek" "$safe_source_one"

mkdir -p "$safe_config/nested source"
expect_fail 'source below config' 'Setup paths overlap' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_kek" "$safe_config/nested source"

ancestor_root=$fixture_dir/'ancestor setup'
mkdir -p \
  "$ancestor_root/config/child" "$ancestor_root/data" "$ancestor_root/restore" \
  "$ancestor_root/secrets" "$ancestor_root/source"
touch "$ancestor_root/secrets/key"
expect_fail 'config below source' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/data" "$ancestor_root/restore" \
  "$ancestor_root/secrets/key" "$ancestor_root/config"

mkdir -p "$ancestor_root/config parent/data child"
expect_fail 'data below config' 'Setup paths overlap' \
  invoke "$ancestor_root/config parent" "$ancestor_root/config parent/data child" \
  "$ancestor_root/restore" "$ancestor_root/secrets/key" "$ancestor_root/source"

mkdir -p "$ancestor_root/data parent/config child"
expect_fail 'config below data' 'Setup paths overlap' \
  invoke "$ancestor_root/data parent/config child" "$ancestor_root/data parent" \
  "$ancestor_root/restore" "$ancestor_root/secrets/key" "$ancestor_root/source"

mkdir -p "$ancestor_root/config restore parent/restore child"
expect_fail 'restore below config' 'Setup paths overlap' \
  invoke "$ancestor_root/config restore parent" "$ancestor_root/data" \
  "$ancestor_root/config restore parent/restore child" "$ancestor_root/secrets/key" \
  "$ancestor_root/source"

mkdir -p "$ancestor_root/restore config parent/config child"
expect_fail 'config below restore' 'Setup paths overlap' \
  invoke "$ancestor_root/restore config parent/config child" "$ancestor_root/data" \
  "$ancestor_root/restore config parent" "$ancestor_root/secrets/key" "$ancestor_root/source"

mkdir -p "$ancestor_root/data source parent/source child"
expect_fail 'source below data' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/data source parent" \
  "$ancestor_root/restore" "$ancestor_root/secrets/key" \
  "$ancestor_root/data source parent/source child"

mkdir -p "$ancestor_root/source data parent/data child"
expect_fail 'data below source' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/source data parent/data child" \
  "$ancestor_root/restore" "$ancestor_root/secrets/key" "$ancestor_root/source data parent"

mkdir -p "$ancestor_root/data/restore child"
expect_fail 'restore below data' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/data" "$ancestor_root/data/restore child" \
  "$ancestor_root/secrets/key" "$ancestor_root/source"

mkdir -p "$ancestor_root/restore parent/data child"
expect_fail 'restore above data' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/restore parent/data child" \
  "$ancestor_root/restore parent" "$ancestor_root/secrets/key" "$ancestor_root/source"

mkdir -p "$ancestor_root/source restore parent/restore child"
expect_fail 'restore below source' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/data" \
  "$ancestor_root/source restore parent/restore child" "$ancestor_root/secrets/key" \
  "$ancestor_root/source restore parent"

mkdir -p "$ancestor_root/restore source parent/source child"
expect_fail 'source below restore' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/data" \
  "$ancestor_root/restore source parent" "$ancestor_root/secrets/key" \
  "$ancestor_root/restore source parent/source child"

mkdir -p "$ancestor_root/source parent/source child"
expect_fail 'second source below first source' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/data" "$ancestor_root/restore" \
  "$ancestor_root/secrets/key" "$ancestor_root/source parent" "$ancestor_root/source parent/source child"
expect_fail 'second source above first source' 'Setup paths overlap' \
  invoke "$ancestor_root/config/child" "$ancestor_root/data" "$ancestor_root/restore" \
  "$ancestor_root/secrets/key" "$ancestor_root/source parent/source child" "$ancestor_root/source parent"

expect_fail 'source contains KEK file' 'Setup paths overlap' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_kek" "$safe_secrets"

touch "$safe_config/config key" "$safe_data/data key" "$safe_restore/restore key"
expect_fail 'config contains KEK file' 'Setup paths overlap' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_config/config key" "$safe_source_one"
expect_fail 'data contains KEK file' 'Setup paths overlap' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_data/data key" "$safe_source_one"
expect_fail 'restore contains KEK file' 'Setup paths overlap' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_restore/restore key" "$safe_source_one"

expect_fail 'filesystem root is too broad' 'Config directory is too broad' \
  invoke / "$safe_data" "$safe_restore" "$safe_kek" "$safe_source_one"

if [ -n "${HOME:-}" ] && [ -d "$HOME" ]; then
  physical_home=$(CDPATH='' cd -P -- "$HOME" && pwd -P)
  expect_fail 'user home is too broad' 'Config directory is too broad' \
    invoke "$physical_home" "$safe_data" "$safe_restore" "$safe_kek" "$safe_source_one"
fi

ln -s "$safe_source_one" "$safe_root/linked source"
expect_fail 'final directory symlink' 'must not be a symlink' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_kek" "$safe_root/linked source"

ln -s "$safe_root" "$fixture_dir/linked setup"
expect_fail 'intermediate directory symlink' 'must be canonical and contain no symlink' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_kek" "$fixture_dir/linked setup/source one"

ln -s "$safe_kek" "$safe_secrets/linked key"
expect_fail 'KEK symlink' 'KEK file must not be a symlink' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_secrets/linked key" "$safe_source_one"

expect_fail 'missing directory' 'does not exist; create it before validation' \
  invoke "$safe_root/missing config" "$safe_data" "$safe_restore" "$safe_kek" "$safe_source_one"
expect_fail 'missing KEK' 'does not exist; provision it before validation' \
  invoke "$safe_config" "$safe_data" "$safe_restore" "$safe_secrets/missing-key" "$safe_source_one"

expect_fail 'relative directory' 'must be an absolute host path' \
  invoke 'relative/config' "$safe_data" "$safe_restore" "$safe_kek" "$safe_source_one"

expect_fail 'missing source option' 'At least one --source is required.' \
  "$validator" --config "$safe_config" --data "$safe_data" \
  --restore "$safe_restore" --kek "$safe_kek"

echo 'Setup path validation fixtures: ok'

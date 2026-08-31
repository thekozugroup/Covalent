#!/bin/sh
set -eu
set -f

usage() {
  cat <<'EOF'
Usage: scripts/validate-setup-paths.sh \
  --config /absolute/config \
  --data /absolute/data \
  --source /absolute/source [--source /absolute/source ...] \
  --restore /absolute/restore \
  --kek /absolute/secrets/covalent-kek

Read-only setup check for Docker, Linux, and macOS host paths. Every path must
already exist, be absolute and canonical, contain no symlink component, and be
separate from every other path. Direct filesystem, user-home, disk, volume,
and aggregate-share roots are refused. This command never creates or changes
files, directories, owners, or permissions.
EOF
}

fail_usage() {
  printf '%s\n' "$1" >&2
  printf '%s\n' "Run scripts/validate-setup-paths.sh --help for a safe example." >&2
  exit 64
}

require_option_value() {
  if [ "$#" -lt 2 ] || [ -z "$2" ]; then
    fail_usage "$1 needs a non-empty path."
  fi
}

check_path_text() {
  asp_text_label=$1
  asp_text_path=$2

  case "$asp_text_path" in
    /*) ;;
    *) fail_usage "$asp_text_label must be an absolute host path: $asp_text_path" ;;
  esac
  if [ "${#asp_text_path}" -gt 4096 ]; then
    fail_usage "$asp_text_label is too long (maximum 4096 characters)."
  fi
  case "$asp_text_path" in
    *'
'*) fail_usage "$asp_text_label must not contain a newline." ;;
  esac
}

canonical_directory() {
  asp_directory_label=$1
  asp_directory_input=$2

  check_path_text "$asp_directory_label" "$asp_directory_input"
  if [ -L "$asp_directory_input" ]; then
    fail_usage "$asp_directory_label must not be a symlink: $asp_directory_input"
  fi
  if [ ! -e "$asp_directory_input" ]; then
    fail_usage "$asp_directory_label does not exist; create it before validation: $asp_directory_input"
  fi
  if [ ! -d "$asp_directory_input" ]; then
    fail_usage "$asp_directory_label must be an existing directory: $asp_directory_input"
  fi

  asp_directory_canonical=$(
    CDPATH='' cd -P -- "$asp_directory_input" 2>/dev/null && pwd -P
  ) || fail_usage "$asp_directory_label cannot be opened for canonical validation: $asp_directory_input"

  if [ "$asp_directory_canonical" != "$asp_directory_input" ]; then
    fail_usage "$asp_directory_label must be canonical and contain no symlink, dot, duplicate, or trailing path components: $asp_directory_input -> $asp_directory_canonical"
  fi
  printf '%s\n' "$asp_directory_canonical"
}

canonical_file() {
  asp_file_label=$1
  asp_file_input=$2

  check_path_text "$asp_file_label" "$asp_file_input"
  if [ -L "$asp_file_input" ]; then
    fail_usage "$asp_file_label must not be a symlink: $asp_file_input"
  fi
  if [ ! -e "$asp_file_input" ]; then
    fail_usage "$asp_file_label does not exist; provision it before validation: $asp_file_input"
  fi
  if [ ! -f "$asp_file_input" ]; then
    fail_usage "$asp_file_label must be an existing regular file: $asp_file_input"
  fi

  asp_file_parent=${asp_file_input%/*}
  asp_file_name=${asp_file_input##*/}
  if [ -z "$asp_file_parent" ]; then
    asp_file_parent=/
  fi
  asp_file_parent_canonical=$(
    CDPATH='' cd -P -- "$asp_file_parent" 2>/dev/null && pwd -P
  ) || fail_usage "$asp_file_label parent cannot be opened for canonical validation: $asp_file_parent"
  if [ "$asp_file_parent_canonical" = / ]; then
    asp_file_canonical=/$asp_file_name
  else
    asp_file_canonical=$asp_file_parent_canonical/$asp_file_name
  fi

  if [ "$asp_file_canonical" != "$asp_file_input" ]; then
    fail_usage "$asp_file_label must be canonical and contain no symlink, dot, duplicate, or trailing path components: $asp_file_input -> $asp_file_canonical"
  fi
  printf '%s\n' "$asp_file_canonical"
}

# Broad roots make a typo capable of exposing a filesystem, home, mounted disk,
# or every Unraid share. /boot is deliberately not listed: it is a supported,
# explicit read-only backup source on Unraid.
is_broad_directory() {
  asp_broad_path=$1
  asp_broad_allow_boot=${2:-no}

  if [ -n "$asp_home_canonical" ] && [ "$asp_broad_path" = "$asp_home_canonical" ]; then
    return 0
  fi
  case "$asp_broad_path" in
    /|/Applications|/Library|/System|/Users|/Volumes|/bin|/dev|/etc|/home|/lib|/lib64|/media|/mnt|/mnt/user|/opt|/private|/private/tmp|/proc|/root|/run|/run/media|/sbin|/srv|/sys|/tmp|/usr|/var)
      return 0
      ;;
    /Users/*|/home/*)
      asp_broad_tail=${asp_broad_path#/*/}
      case "$asp_broad_tail" in
        */*) ;;
        *) return 0 ;;
      esac
      ;;
    /Volumes/*|/mnt/*)
      asp_broad_tail=${asp_broad_path#/*/}
      case "$asp_broad_tail" in
        */*) ;;
        *) return 0 ;;
      esac
      ;;
    /media/*/*)
      asp_broad_tail=${asp_broad_path#/media/}
      case "$asp_broad_tail" in
        */*/*) ;;
        *) return 0 ;;
      esac
      ;;
    /run/media/*/*)
      asp_broad_tail=${asp_broad_path#/run/media/}
      case "$asp_broad_tail" in
        */*/*) ;;
        *) return 0 ;;
      esac
      ;;
    /*)
      asp_broad_tail=${asp_broad_path#/}
      case "$asp_broad_tail" in
        */*) ;;
        boot)
          [ "$asp_broad_allow_boot" = yes ] || return 0
          ;;
        *) return 0 ;;
      esac
      ;;
  esac
  return 1
}

paths_overlap() {
  asp_overlap_left=$1
  asp_overlap_right=$2
  case "$asp_overlap_left" in
    "$asp_overlap_right"|"$asp_overlap_right"/*) return 0 ;;
  esac
  case "$asp_overlap_right" in
    "$asp_overlap_left"|"$asp_overlap_left"/*) return 0 ;;
  esac
  return 1
}

add_path() {
  asp_add_label=$1
  asp_add_path=$2

  asp_add_saved_ifs=$IFS
  IFS='
'
  # Entries are intentionally split only on newline. Newlines were rejected,
  # and glob expansion is disabled, so spaces and shell metacharacters stay data.
  # shellcheck disable=SC2086
  for asp_entry in $asp_entries; do
    asp_existing_label=${asp_entry%%|*}
    asp_existing_path=${asp_entry#*|}
    if paths_overlap "$asp_add_path" "$asp_existing_path"; then
      IFS=$asp_add_saved_ifs
      printf '%s\n' "Setup paths overlap: $asp_add_label and $asp_existing_label." >&2
      printf '%s\n' "  $asp_add_label: $asp_add_path" >&2
      printf '%s\n' "  $asp_existing_label: $asp_existing_path" >&2
      printf '%s\n' "Choose separate sibling paths. Config, data, sources, restore, and KEK must never contain one another." >&2
      exit 64
    fi
  done
  IFS=$asp_add_saved_ifs

  if [ -n "$asp_entries" ]; then
    asp_entries="$asp_entries
$asp_add_label|$asp_add_path"
  else
    asp_entries=$asp_add_label\|$asp_add_path
  fi
}

asp_config_input=
asp_data_input=
asp_restore_input=
asp_kek_input=
asp_source_inputs=
asp_source_count=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --config)
      require_option_value "$@"
      [ -z "$asp_config_input" ] || fail_usage "--config may be supplied only once."
      asp_config_input=$2
      shift 2
      ;;
    --data)
      require_option_value "$@"
      [ -z "$asp_data_input" ] || fail_usage "--data may be supplied only once."
      asp_data_input=$2
      shift 2
      ;;
    --source)
      require_option_value "$@"
      asp_source_count=$((asp_source_count + 1))
      [ "$asp_source_count" -le 32 ] || fail_usage "At most 32 --source paths are allowed."
      check_path_text "source directory $asp_source_count" "$2"
      if [ -n "$asp_source_inputs" ]; then
        asp_source_inputs="$asp_source_inputs
$2"
      else
        asp_source_inputs=$2
      fi
      shift 2
      ;;
    --restore)
      require_option_value "$@"
      [ -z "$asp_restore_input" ] || fail_usage "--restore may be supplied only once."
      asp_restore_input=$2
      shift 2
      ;;
    --kek)
      require_option_value "$@"
      [ -z "$asp_kek_input" ] || fail_usage "--kek may be supplied only once."
      asp_kek_input=$2
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      fail_usage "Unknown setup-path option: $1"
      ;;
  esac
done

[ -n "$asp_config_input" ] || fail_usage "--config is required."
[ -n "$asp_data_input" ] || fail_usage "--data is required."
[ "$asp_source_count" -gt 0 ] || fail_usage "At least one --source is required."
[ -n "$asp_restore_input" ] || fail_usage "--restore is required."
[ -n "$asp_kek_input" ] || fail_usage "--kek is required."

asp_home_canonical=
if [ -n "${HOME:-}" ] && [ -d "$HOME" ]; then
  asp_home_canonical=$(CDPATH='' cd -P -- "$HOME" 2>/dev/null && pwd -P) || asp_home_canonical=
fi

asp_entries=

asp_config=$(canonical_directory "config directory" "$asp_config_input")
if is_broad_directory "$asp_config" no; then
  fail_usage "Config directory is too broad; choose a dedicated child directory: $asp_config"
fi
add_path "config directory" "$asp_config"

asp_data=$(canonical_directory "data directory" "$asp_data_input")
if is_broad_directory "$asp_data" no; then
  fail_usage "Data directory is too broad; choose a dedicated child directory: $asp_data"
fi
add_path "data directory" "$asp_data"

asp_sources_saved_ifs=$IFS
IFS='
'
asp_source_index=0
# shellcheck disable=SC2086
for asp_source_input in $asp_source_inputs; do
  asp_source_index=$((asp_source_index + 1))
  asp_source_label="source directory $asp_source_index"
  asp_source=$(canonical_directory "$asp_source_label" "$asp_source_input")
  if is_broad_directory "$asp_source" yes; then
    IFS=$asp_sources_saved_ifs
    fail_usage "Source directory $asp_source_index is too broad; choose one explicit child directory or share: $asp_source"
  fi
  add_path "$asp_source_label" "$asp_source"
done
IFS=$asp_sources_saved_ifs

asp_restore=$(canonical_directory "restore directory" "$asp_restore_input")
if is_broad_directory "$asp_restore" no; then
  fail_usage "Restore directory is too broad; choose a dedicated child directory: $asp_restore"
fi
add_path "restore directory" "$asp_restore"

asp_kek=$(canonical_file "KEK file" "$asp_kek_input")
asp_kek_parent=${asp_kek%/*}
[ -n "$asp_kek_parent" ] || asp_kek_parent=/
if is_broad_directory "$asp_kek_parent" no; then
  fail_usage "KEK file parent is too broad; choose a dedicated secrets directory: $asp_kek_parent"
fi
add_path "KEK file" "$asp_kek"

printf '%s\n' "Setup paths safe: $asp_source_count source path(s), no overlap, no symlinks, no broad roots."
printf '%s\n' "Read-only validation complete; nothing was created or changed."

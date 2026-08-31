#!/usr/bin/env bash
set -euo pipefail

if (( $# < 2 )); then
  echo "usage: android-source-fingerprint.sh <repo-root> <input-path>..." >&2
  exit 2
fi

repo_root=$1
shift
input_paths=("$@")

if ! git -C "$repo_root" rev-parse --git-dir >/dev/null 2>&1; then
  echo "Android source fingerprint requires a git checkout: $repo_root" >&2
  exit 1
fi

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | awk '{print $1}'
  else
    shasum -a 256 -- "$1" | awk '{print $1}'
  fi
}

hash_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

path_hex() {
  LC_ALL=C od -An -v -tx1 | tr -d ' \n'
}

file_mode() {
  if stat -f '%Lp' "$1" >/dev/null 2>&1; then
    stat -f '%Lp' "$1"
  else
    stat -c '%a' "$1"
  fi
}

tmp_root=${TMPDIR:-/tmp}
case "$tmp_root" in
  /*) ;;
  *) tmp_root=/tmp ;;
esac
work=$(mktemp -d "$tmp_root/covalent-android-fingerprint.XXXXXX")
cleanup() { rm -rf "$work"; }
trap cleanup EXIT INT TERM

snapshot() {
  label=$1
  list_file="$work/paths-$label"

  head_before=$(git -C "$repo_root" rev-parse HEAD)
  status_before=$(git -C "$repo_root" status --porcelain=v1 -z \
    --untracked-files=all -- "${input_paths[@]}" | path_hex)
  git -C "$repo_root" ls-files -z --cached --others --exclude-standard \
    -- "${input_paths[@]}" > "$list_file"

  printf 'format\tandroid-source-fingerprint-v2\n'
  printf 'tool\t%s\n' "$(hash_file "$0")"
  printf 'head\t%s\n' "$head_before"
  printf 'status-z\t%s\n' "$status_before"

  while IFS= read -r -d '' relative_path; do
    absolute_path="$repo_root/$relative_path"
    encoded_path=$(printf '%s' "$relative_path" | path_hex)

    if [[ -L "$absolute_path" ]]; then
      mode=$(file_mode "$absolute_path")
      link_digest=$(readlink "$absolute_path" | hash_stdin)
      if [[ -e "$absolute_path" ]]; then
        content_digest=$(hash_file "$absolute_path")
      else
        content_digest=broken
      fi
      printf 'input\tsymlink\t%s\t%s:%s\t%s\n' \
        "$mode" "$link_digest" "$content_digest" "$encoded_path"
    elif [[ -f "$absolute_path" ]]; then
      mode=$(file_mode "$absolute_path")
      printf 'input\tfile\t%s\t%s\t%s\n' \
        "$mode" "$(hash_file "$absolute_path")" "$encoded_path"
    elif [[ ! -e "$absolute_path" ]]; then
      # Deleted tracked paths remain in `git ls-files --cached`; the explicit
      # marker makes deletion part of the manifest instead of silently dropping
      # the path that the prior build consumed.
      printf 'input\tmissing\t-\t-\t%s\n' "$encoded_path"
    else
      echo "unsupported Android build input type: $relative_path" >&2
      return 1
    fi
  done < "$list_file"

  head_after=$(git -C "$repo_root" rev-parse HEAD)
  status_after=$(git -C "$repo_root" status --porcelain=v1 -z \
    --untracked-files=all -- "${input_paths[@]}" | path_hex)
  if [[ "$head_before" != "$head_after" || "$status_before" != "$status_after" ]]; then
    echo "Android source changed while its fingerprint was being read" >&2
    return 1
  fi
}

first="$work/first"
second="$work/second"
if ! snapshot first > "$first" || ! snapshot second > "$second"; then
  echo "Could not produce a stable Android source fingerprint" >&2
  exit 1
fi
if ! cmp -s "$first" "$second"; then
  echo "Android source content changed while its fingerprint was being read" >&2
  exit 1
fi
cat "$first"

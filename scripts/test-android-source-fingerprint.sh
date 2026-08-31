#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fingerprint_tool="$repo_root/scripts/android-source-fingerprint.sh"

tmp_root=${TMPDIR:-/tmp}
case "$tmp_root" in
  /*) ;;
  *) tmp_root=/tmp ;;
esac
fixture=$(mktemp -d "$tmp_root/covalent-android-fingerprint-test.XXXXXX")
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT INT TERM

git -C "$fixture" init -q
mkdir -p "$fixture/apps/android" "$fixture/crates/example/src"
printf '*.ignored\n' > "$fixture/apps/android/.gitignore"
printf 'base\n' > "$fixture/crates/example/src/lib.rs"
printf '[workspace]\nmembers = []\n' > "$fixture/Cargo.toml"
printf '# lock\n' > "$fixture/Cargo.lock"
git -C "$fixture" add .
git -C "$fixture" -c user.name=Covalent -c user.email=release@covalent.invalid \
  commit -qm fixture

inputs=(apps/android crates Cargo.toml Cargo.lock)
fingerprint() {
  "$fingerprint_tool" "$fixture" "${inputs[@]}"
}

# This is the bug that the old HEAD + porcelain manifest missed: once a tracked
# path is dirty, changing its bytes again leaves the exact same ` M path` status.
printf 'dirty-one\n' >> "$fixture/crates/example/src/lib.rs"
tracked_status_before=$(git -C "$fixture" status --porcelain=v1 -- "${inputs[@]}")
fingerprint > "$fixture/tracked-before"
printf 'dirty-two\n' >> "$fixture/crates/example/src/lib.rs"
tracked_status_after=$(git -C "$fixture" status --porcelain=v1 -- "${inputs[@]}")
fingerprint > "$fixture/tracked-after"
test "$tracked_status_before" = "$tracked_status_after"
if cmp -s "$fixture/tracked-before" "$fixture/tracked-after"; then
  echo "Android fingerprint missed a second mutation of an already-dirty tracked input" >&2
  exit 1
fi

# Untracked, nonignored source is also a build input and has the same stable
# porcelain shape while its contents change.
printf 'one\n' > "$fixture/apps/android/NewSource.kt"
untracked_status_before=$(git -C "$fixture" status --porcelain=v1 -- "${inputs[@]}")
fingerprint > "$fixture/untracked-before"
printf 'two\n' > "$fixture/apps/android/NewSource.kt"
untracked_status_after=$(git -C "$fixture" status --porcelain=v1 -- "${inputs[@]}")
fingerprint > "$fixture/untracked-after"
test "$untracked_status_before" = "$untracked_status_after"
if cmp -s "$fixture/untracked-before" "$fixture/untracked-after"; then
  echo "Android fingerprint missed a mutation of an untracked nonignored input" >&2
  exit 1
fi

# Ignored output does not belong to the source manifest.
printf 'generated-one\n' > "$fixture/apps/android/cache.ignored"
fingerprint > "$fixture/ignored-before"
printf 'generated-two\n' > "$fixture/apps/android/cache.ignored"
fingerprint > "$fixture/ignored-after"
if ! cmp -s "$fixture/ignored-before" "$fixture/ignored-after"; then
  echo "Android fingerprint included an ignored generated file" >&2
  exit 1
fi

# File mode and deletion are explicit manifest state, not incidental mtimes.
fingerprint > "$fixture/mode-before"
chmod +x "$fixture/Cargo.toml"
fingerprint > "$fixture/mode-after"
if cmp -s "$fixture/mode-before" "$fixture/mode-after"; then
  echo "Android fingerprint missed a build-input mode change" >&2
  exit 1
fi
fingerprint > "$fixture/delete-before"
rm "$fixture/Cargo.lock"
fingerprint > "$fixture/delete-after"
if cmp -s "$fixture/delete-before" "$fixture/delete-after"; then
  echo "Android fingerprint missed deletion of a tracked build input" >&2
  exit 1
fi

echo "Android source fingerprint mutation contract: ok"

#!/bin/sh
# Container updates may reuse native acceptance only while native inputs match
# the signed full release. Container/web changes receive fresh image checks.
set -eu
baseline=${1:?usage: check-container-update-source.sh BASELINE SOURCE}
source=${2:?usage: check-container-update-source.sh BASELINE SOURCE}
git merge-base --is-ancestor "$baseline" "$source" || {
  echo 'container update must descend from its full release baseline' >&2
  exit 1
}
git diff --quiet "$baseline" "$source" -- \
  Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml deny.toml \
  crates apps packaging/rclone packaging/sync-engine \
  scripts/build-linux-sync-engine.sh scripts/collect-sync-engine-notices.py \
  scripts/collect-go-target-license-inventory.py \
  ':(glob)scripts/*android*' ':(glob)scripts/*macos*' ':(glob)scripts/*apple*' || {
  echo 'native or shared runtime changed; publish and validate a full release first' >&2
  exit 1
}
# Reject additions outside the known container, documentation and tooling areas.
git diff --name-only "$baseline" "$source" | while IFS= read -r path; do
  case "$path" in
    packaging/docker/*|packaging/unraid/*|packaging/web/*|docs/*|scripts/*|.github/*|*.md|.dockerignore|.gitignore|Makefile) ;;
    *) echo "change requires full release acceptance: $path" >&2; exit 1 ;;
  esac
done
printf 'Native source matches %s; container checks must run on %s.\n' "$baseline" "$source"

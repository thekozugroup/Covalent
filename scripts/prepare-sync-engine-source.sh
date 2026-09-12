#!/bin/sh
# Prepare an immutable upstream checkout for local native package builds.
set -eu
test "$#" -eq 1 || { echo "usage: $0 CACHE-DIRECTORY" >&2; exit 64; }
destination=$1
expected=946e2b83a1f6c6ae119427c09e0a5802940b82ff
case "$destination" in
  /*) ;;
  *) echo "engine source cache must be an absolute path" >&2; exit 64 ;;
esac
if [ -L "$destination" ]; then
  echo "engine source cache must not be a symbolic link" >&2
  exit 1
fi
if [ ! -e "$destination" ]; then
  parent=$(dirname -- "$destination")
  mkdir -p "$parent"
  # Exclusive creation wins before any writes. A failed download leaves an
  # incomplete owned cache, which the final commit check refuses on reopen.
  mkdir -m 0700 "$destination"
  git -C "$destination" init --quiet
  git -C "$destination" -c credential.helper= fetch --quiet --depth=1 \
    https://github.com/syncthing/syncthing.git "$expected"
  git -C "$destination" checkout --quiet --detach FETCH_HEAD
fi
test -d "$destination" && \
  test "$(git -C "$destination" rev-parse HEAD)" = "$expected" || {
  echo "engine source cache does not contain the pinned commit" >&2
  exit 1
}
# The builder exports the commit itself and never compiles mutable worktree files.
echo "Pinned sync-engine source is ready."

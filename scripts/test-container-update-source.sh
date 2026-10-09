#!/bin/sh
set -eu
checker=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/check-container-update-source.sh
sandbox=$(mktemp -d)
trap 'rm -rf "$sandbox"' EXIT INT TERM
cd "$sandbox"
git init -q
git config user.name test
git config user.email test@example.invalid
git config commit.gpgsign false
mkdir -p crates/core apps/mac packaging/web
printf baseline > crates/core/lib.rs
printf baseline > apps/mac/App.swift
printf baseline > packaging/web/app.js
git add .
git commit -qm baseline
baseline=$(git rev-parse HEAD)
printf changed > packaging/web/app.js
git commit -qam web
"$checker" "$baseline" HEAD
printf changed > crates/core/lib.rs
git commit -qam core
if "$checker" "$baseline" HEAD >/dev/null 2>&1; then
  echo 'shared runtime change was incorrectly accepted' >&2; exit 1
fi
git checkout -q "$baseline" -- crates/core/lib.rs
git commit -qm revert-core
printf changed > apps/mac/App.swift
git commit -qam mac
if "$checker" "$baseline" HEAD >/dev/null 2>&1; then
  echo 'native app change was incorrectly accepted' >&2; exit 1
fi
git checkout -q "$baseline" -- apps/mac/App.swift
git commit -qm revert-mac
printf changed > unknown-runtime.bin
git add unknown-runtime.bin
git commit -qm unknown-runtime
if "$checker" "$baseline" HEAD >/dev/null 2>&1; then
  echo 'unknown runtime input was incorrectly accepted' >&2; exit 1
fi
printf 'Container source reuse guard passed.\n'

#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fingerprint_tool="$repo_root/scripts/docker-source-fingerprint.sh"

tmp_root=${TMPDIR:-/tmp}
case "$tmp_root" in
  /*) ;;
  *) tmp_root=/tmp ;;
esac
fixture=$(mktemp -d "$tmp_root/covalent-docker-fingerprint-test.XXXXXX")
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT INT TERM

git -C "$fixture" init -q
mkdir -p \
  "$fixture/crates/example/src" \
  "$fixture/packaging/web" \
  "$fixture/packaging/docker/caddy" \
  "$fixture/target"
printf 'target\n' > "$fixture/.dockerignore"
printf '[workspace]\nmembers = []\n' > "$fixture/Cargo.toml"
printf '# lock\n' > "$fixture/Cargo.lock"
printf '[toolchain]\nchannel = "1.97.1"\n' > "$fixture/rust-toolchain.toml"
printf 'pub fn node() {}\n' > "$fixture/crates/example/src/lib.rs"
printf 'body {}\n' > "$fixture/packaging/web/app.css"
printf 'FROM scratch\n' > "$fixture/packaging/docker/Dockerfile"
printf '{}\n' > "$fixture/packaging/docker/Caddyfile"
printf '#!/bin/sh\n' > "$fixture/packaging/docker/entrypoint.sh"
printf 'module caddy\n' > "$fixture/packaging/docker/caddy/go.mod"
printf 'package main\n' > "$fixture/packaging/docker/caddy/main.go"
git -C "$fixture" add .
git -C "$fixture" -c user.name=Covalent -c user.email=release@covalent.invalid \
  commit -qm fixture

fingerprint() {
  "$fingerprint_tool" "$fixture"
}

source_status() {
  git -C "$fixture" status --porcelain=v1 -- \
    .dockerignore Cargo.toml Cargo.lock rust-toolchain.toml crates packaging
}

# The stale-image regression: mutating a tracked Rust input again after it is
# already dirty must change the source identity even though HEAD and porcelain
# do not.
printf 'dirty-one\n' >> "$fixture/crates/example/src/lib.rs"
rust_status_before=$(source_status)
fingerprint > "$fixture/rust-before"
printf 'dirty-two\n' >> "$fixture/crates/example/src/lib.rs"
rust_status_after=$(source_status)
fingerprint > "$fixture/rust-after"
test "$rust_status_before" = "$rust_status_after"
if cmp -s "$fixture/rust-before" "$fixture/rust-after"; then
  echo "Docker fingerprint missed a second mutation of an already-dirty Rust input" >&2
  exit 1
fi

# Dockerfile bytes directly control image construction and have the same
# unchanged-HEAD/porcelain shape once dirty.
printf '# dirty-one\n' >> "$fixture/packaging/docker/Dockerfile"
container_status_before=$(source_status)
fingerprint > "$fixture/container-before"
printf '# dirty-two\n' >> "$fixture/packaging/docker/Dockerfile"
container_status_after=$(source_status)
fingerprint > "$fixture/container-after"
test "$container_status_before" = "$container_status_after"
if cmp -s "$fixture/container-before" "$fixture/container-after"; then
  echo "Docker fingerprint missed a second mutation of an already-dirty container input" >&2
  exit 1
fi

# COPY packaging/web includes new nonignored browser assets, so their bytes
# must also make a prebuilt image stale without relying on porcelain alone.
printf 'one\n' > "$fixture/packaging/web/new.js"
web_status_before=$(source_status)
fingerprint > "$fixture/web-before"
printf 'two\n' > "$fixture/packaging/web/new.js"
web_status_after=$(source_status)
fingerprint > "$fixture/web-after"
test "$web_status_before" = "$web_status_after"
if cmp -s "$fixture/web-before" "$fixture/web-after"; then
  echo "Docker fingerprint missed an untracked nonignored web build input" >&2
  exit 1
fi

# Ignored build/cache output must not make an image stale.
printf 'one\n' > "$fixture/target/cache"
fingerprint > "$fixture/ignored-before"
printf 'two\n' > "$fixture/target/cache"
fingerprint > "$fixture/ignored-after"
if ! cmp -s "$fixture/ignored-before" "$fixture/ignored-after"; then
  echo "Docker fingerprint included ignored output" >&2
  exit 1
fi

echo "Docker source fingerprint mutation contract: ok"

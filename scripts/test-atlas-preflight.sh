#!/bin/sh
set -eu

# The same file acts as bounded command fixtures through temporary symlinks.
# This keeps the test hermetic without contacting Atlas or changing Docker.
case "${0##*/}" in
  docker)
    exit 0
    ;;
  tailscale)
    [ "${1:-}" = status ] || exit 64
    printf '%s\n' '{}'
    exit 0
    ;;
  getent)
    printf '%s\n' '100.64.0.1 atlas.example-tailnet.ts.net'
    exit 0
    ;;
  hostname)
    printf '%s\n' atlas
    exit 0
    ;;
  ss)
    case "$*" in
      *:8443*)
        if [ "${COVALENT_FIXTURE_TCP_OCCUPIED:-}" = 1 ]; then
          printf '%s\n' 'LISTEN 0 4096 0.0.0.0:8443 0.0.0.0:*'
        fi
        ;;
      *:8787*)
        if [ "${COVALENT_FIXTURE_UDP_OCCUPIED:-}" = 1 ]; then
          printf '%s\n' 'UNCONN 0 0 0.0.0.0:8787 0.0.0.0:*'
        fi
        ;;
      *) exit 64 ;;
    esac
    exit 0
    ;;
  ssh)
    echo "fixture SSH must not be reached for rejected local paths" >&2
    exit 99
    ;;
esac

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
remote="$repo_root/scripts/atlas-preflight-remote.sh"
preflight="$repo_root/scripts/atlas-preflight.sh"
validator="$repo_root/scripts/validate-unraid-template.sh"
template="$repo_root/packaging/unraid/covalent.xml"
fixture_base=${TMPDIR:-/tmp}
fixture_base=${fixture_base%/}
fixture_dir=$(mktemp -d "$fixture_base/covalent-atlas-preflight.XXXXXX")
fixture_dir=$(realpath -- "$fixture_dir")
cleanup() {
  chmod -R u+rwx "$fixture_dir" 2>/dev/null || true
  rm -rf "$fixture_dir"
}
trap cleanup EXIT HUP INT TERM

fake_bin="$fixture_dir/bin"
source_root="$fixture_dir/mnt/user"
mkdir -p "$fake_bin" "$source_root/Photos" "$source_root/system/covalent-secrets"
touch "$source_root/Photos/example"
for command_name in docker tailscale getent hostname ss ssh; do
  ln -s "$repo_root/scripts/test-atlas-preflight.sh" "$fake_bin/$command_name"
done

fixture_path="$fake_bin:/usr/bin:/bin"
digest='sha256:0000000000000000000000000000000000000000000000000000000000000000'

PATH="$fixture_path" "$remote" "$digest" atlas.example-tailnet.ts.net "$source_root" "$source_root/Photos" \
  > "$fixture_dir/success.out"
grep -Fq 'Ports: TCP 8443 and UDP 8787 are free' "$fixture_dir/success.out"
grep -Fq '1 canonical directories opened without writes for inspection' "$fixture_dir/success.out"

if COVALENT_FIXTURE_TCP_OCCUPIED=1 PATH="$fixture_path" \
  "$remote" "$digest" atlas.example-tailnet.ts.net "$source_root" "$source_root/Photos" \
  > "$fixture_dir/tcp.out" 2>&1; then
  echo "occupied TCP 8443 fixture unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'TCP 8443 is already occupied' "$fixture_dir/tcp.out"

if COVALENT_FIXTURE_UDP_OCCUPIED=1 PATH="$fixture_path" \
  "$remote" "$digest" atlas.example-tailnet.ts.net "$source_root" "$source_root/Photos" \
  > "$fixture_dir/udp.out" 2>&1; then
  echo "occupied UDP 8787 fixture unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'UDP 8787 is already occupied' "$fixture_dir/udp.out"

ln -s "$source_root/system" "$source_root/ReservedEscape"
if PATH="$fixture_path" "$remote" "$digest" atlas.example-tailnet.ts.net "$source_root" "$source_root/ReservedEscape" \
  > "$fixture_dir/symlink.out" 2>&1; then
  echo "symlinked reserved-source fixture unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'must be canonical and contain no symlink components' "$fixture_dir/symlink.out"

if PATH="$fixture_path" "$remote" "$digest" atlas.example-tailnet.ts.net "$source_root" \
  "$source_root/system/covalent-secrets" > "$fixture_dir/remote-secret.out" 2>&1; then
  echo "reserved KEK source fixture unexpectedly passed the remote check" >&2
  exit 1
fi
grep -Fq 'source is a reserved state or secret share' "$fixture_dir/remote-secret.out"

if PATH="$fixture_path" "$remote" "$digest" atlas.example-tailnet.ts.net "$source_root/system" \
  "$source_root/system/covalent-secrets" > "$fixture_dir/root-bypass.out" 2>&1; then
  echo "reserved system share was accepted as the source root" >&2
  exit 1
fi
grep -Fq 'source root must be the Unraid /mnt/user boundary' "$fixture_dir/root-bypass.out"

if PATH="$fixture_path" "$preflight" \
  --ssh operator@atlas.example-tailnet.ts.net \
  --source /mnt/user/Photos/../system > "$fixture_dir/dot.out" 2>&1; then
  echo "dot-segment source fixture unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'source must be normalized' "$fixture_dir/dot.out"
if grep -Fq 'fixture SSH must not be reached' "$fixture_dir/dot.out"; then
  echo "unsafe source reached SSH" >&2
  exit 1
fi

if PATH="$fixture_path" "$preflight" \
  --ssh operator@atlas.example-tailnet.ts.net \
  --source /mnt/user/system/covalent-secrets > "$fixture_dir/local-secret.out" 2>&1; then
  echo "reserved Atlas KEK source unexpectedly passed the local check" >&2
  exit 1
fi
grep -Fq 'source must never be the reserved system or appdata share' "$fixture_dir/local-secret.out"
if grep -Fq 'fixture SSH must not be reached' "$fixture_dir/local-secret.out"; then
  echo "reserved Atlas KEK source reached SSH" >&2
  exit 1
fi

"$validator" "$template" >/dev/null
sed 's|Target="/source" Default="" Mode="ro"|Target="/source" Default="" Mode="rw"|' \
  "$template" > "$fixture_dir/source-rw.xml"
if "$validator" "$fixture_dir/source-rw.xml" > "$fixture_dir/rw.out" 2>&1; then
  echo "writable source-mount fixture unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'Unraid mapping /source must use mode ro' "$fixture_dir/rw.out"

echo "Atlas preflight fixtures: ok"

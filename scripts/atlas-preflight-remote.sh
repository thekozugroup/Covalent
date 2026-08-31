#!/bin/sh
# Runs on Atlas over stdin from atlas-preflight.sh. This script is deliberately
# read-only: it opens each source directory for a bounded listing and inspects
# host services/ports, but never creates, chmods, mounts, or starts anything.
set -eu

if [ "$#" -lt 4 ]; then
  echo "remote preflight requires DIGEST ATLAS_HOST SOURCE_ROOT SOURCE..." >&2
  exit 64
fi

digest=$1
atlas_host=$2
source_root=$3
shift 3

if ! printf '%s\n' "$digest" | grep -Eq '^sha256:[0-9a-f]{64}$'; then
  echo "invalid release digest passed to remote preflight" >&2
  exit 64
fi

case "$atlas_host" in
  ''|.*|-*|*.|*..*|*[!A-Za-z0-9.-]*)
    echo "Atlas host must be a normalized DNS name" >&2
    exit 64
    ;;
esac
[ "${#atlas_host}" -le 253 ] || { echo "Atlas host is too long" >&2; exit 64; }

# The wrapper always passes the Unraid user-share root. Keep the helper safe
# when invoked directly as well: changing the root to the reserved system
# share must not turn that share into an apparently ordinary source.
case "$source_root" in
  /mnt/user|*/mnt/user) ;;
  *) echo "source root must be the Unraid /mnt/user boundary: $source_root" >&2; exit 64 ;;
esac
case "$source_root" in
  */../*|*/..|*/./*|*/.|*//*|/)
    echo "source root must be normalized: $source_root" >&2
    exit 64
    ;;
esac

command -v docker >/dev/null 2>&1 || { echo "Docker is unavailable" >&2; exit 1; }
command -v tailscale >/dev/null 2>&1 || { echo "Tailscale is unavailable" >&2; exit 1; }
command -v ss >/dev/null 2>&1 || { echo "ss is unavailable" >&2; exit 1; }
command -v find >/dev/null 2>&1 || { echo "find is unavailable" >&2; exit 1; }

if command -v readlink >/dev/null 2>&1 && readlink -e -- / >/dev/null 2>&1; then
  canonicalize() { readlink -e -- "$1"; }
elif command -v realpath >/dev/null 2>&1; then
  canonicalize() { realpath -- "$1"; }
else
  echo "GNU readlink -e or realpath is required" >&2
  exit 1
fi

tailscale status --json >/dev/null
getent hosts "$atlas_host" >/dev/null
docker version --format '{{.Server.Version}}' >/dev/null
docker info --format '{{.Driver}} {{.DockerRootDir}}' >/dev/null

canonical_root=$(canonicalize "$source_root") || {
  echo "source root does not resolve: $source_root" >&2
  exit 1
}
[ "$canonical_root" = "$source_root" ] || {
  echo "source root must be canonical and contain no symlink components: $source_root -> $canonical_root" >&2
  exit 1
}
[ -d "$canonical_root" ] || { echo "source root is not a directory: $source_root" >&2; exit 1; }

checked=0
for source in "$@"; do
  case "$source" in
    "$source_root"/*) ;;
    *) echo "source escapes the approved root $source_root: $source" >&2; exit 1 ;;
  esac
  case "$source" in
    */../*|*/..|*/./*|*/.|*//*|*/)
      echo "source must be a normalized path without dot, duplicate, or trailing segments: $source" >&2
      exit 1
      ;;
  esac

  canonical=$(canonicalize "$source") || {
    echo "source does not resolve: $source" >&2
    exit 1
  }
  if [ "$canonical" != "$source" ]; then
    echo "source must be canonical and contain no symlink components: $source -> $canonical" >&2
    exit 1
  fi
  case "$canonical" in
    "$canonical_root"/*) ;;
    *) echo "canonical source escapes the approved root: $source -> $canonical" >&2; exit 1 ;;
  esac

  relative=${canonical#"$canonical_root"/}
  case "$relative" in
    system|system/*|appdata|appdata/*)
      echo "source is a reserved state or secret share: $source" >&2
      exit 1
      ;;
  esac
  [ -d "$canonical" ] || { echo "source is not a directory: $source" >&2; exit 1; }

  # This actually opens the directory under the SSH identity. It is bounded to
  # one entry and never reads file contents. Read-only mount policy is checked
  # separately against the exact Unraid deployment template on the client.
  if ! find "$canonical" -mindepth 1 -maxdepth 1 -print -quit >/dev/null; then
    echo "source directory cannot be opened for reading: $source" >&2
    exit 1
  fi
  checked=$((checked + 1))
done

tcp_listeners=$(ss -H -lnt 'sport = :8443') || {
  echo "could not inspect TCP 8443 listeners" >&2
  exit 1
}
if [ -n "$tcp_listeners" ]; then
  echo "TCP 8443 is already occupied; listener follows:" >&2
  printf '%s\n' "$tcp_listeners" >&2
  exit 1
fi

udp_listeners=$(ss -H -lnu 'sport = :8787') || {
  echo "could not inspect UDP 8787 listeners" >&2
  exit 1
}
if [ -n "$udp_listeners" ]; then
  echo "UDP 8787 is already occupied; listener follows:" >&2
  printf '%s\n' "$udp_listeners" >&2
  exit 1
fi

echo "Atlas host: $(hostname)"
echo "Tailscale MagicDNS: $atlas_host resolves"
echo "Docker: available; release digest to install later is $digest"
echo "Ports: TCP 8443 and UDP 8787 are free"
echo "Explicit source shares: $checked canonical directories opened without writes for inspection"

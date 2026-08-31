#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
template="$repo_root/packaging/unraid/covalent.xml"
remote_helper="$repo_root/scripts/atlas-preflight-remote.sh"
ssh_target=''
atlas_host=''
sources=''
source_count=0

usage() {
  cat <<'EOF'
Usage: scripts/atlas-preflight.sh --ssh USER@ATLAS_HOST --source /host/path [--source /host/path]

Read-only Atlas readiness checks. It reads the immutable release digest, then
uses SSH with strict existing host-key verification to inspect Docker,
Tailscale, canonical source directories, the exact read-only Unraid mount plan,
and required free ports. It never pulls, creates, changes, or deploys anything.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --ssh)
      [ "$#" -ge 2 ] || { usage >&2; exit 64; }
      ssh_target=$2
      shift 2
      ;;
    --source)
      [ "$#" -ge 2 ] || { usage >&2; exit 64; }
      [ "${#2}" -le 4096 ] || { echo "source path is too long" >&2; exit 64; }
      case "$2" in
        /*) ;;
        *) echo "source must be an absolute host path: $2" >&2; exit 64 ;;
      esac
      case "$2" in
        *[!A-Za-z0-9_./-]*) echo "source contains unsafe characters: $2" >&2; exit 64 ;;
      esac
      case "$2" in
        */../*|*/..|*/./*|*/.|*//*|*/)
          echo "source must be normalized without dot, duplicate, or trailing segments: $2" >&2
          exit 64
          ;;
      esac
      case "$2" in
        /mnt/user/?*) ;;
        *) echo "source must be one explicit share below /mnt/user: $2" >&2; exit 64 ;;
      esac
      relative=${2#/mnt/user/}
      case "$relative" in
        system|system/*|appdata|appdata/*)
          echo "source must never be the reserved system or appdata share: $2" >&2
          exit 64
          ;;
      esac
      source_count=$((source_count + 1))
      [ "$source_count" -le 32 ] || { echo "at most 32 explicit sources are allowed" >&2; exit 64; }
      sources="${sources}
$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 64
      ;;
  esac
done

case "$ssh_target" in
  *@*@*|@*|*@|'')
    echo "--ssh must be exactly USER@ATLAS_HOST" >&2
    exit 64
    ;;
  *@*) ;;
  *)
    echo "--ssh must be exactly USER@ATLAS_HOST" >&2
    exit 64
    ;;
esac
ssh_user=${ssh_target%@*}
atlas_host=${ssh_target#*@}
case "$ssh_user" in
  ''|.*|-*|*[!A-Za-z0-9_.-]*)
    echo "--ssh user contains unsafe characters" >&2
    exit 64
    ;;
esac
case "$atlas_host" in
  ''|.*|-*|*.|*..*|*[!A-Za-z0-9.-]*)
    echo "--ssh host must be a normalized DNS name" >&2
    exit 64
    ;;
esac
[ "${#atlas_host}" -le 253 ] || { echo "--ssh host is too long" >&2; exit 64; }
[ -n "$sources" ] || { echo "at least one explicit --source is required" >&2; exit 64; }
[ -x "$remote_helper" ] || { echo "Atlas remote preflight helper is missing or not executable" >&2; exit 1; }

digest=$(sed -n 's|^[[:space:]]*<Repository>ghcr\.io/thekozugroup/covalent@\(sha256:[0-9a-f]\{64\}\)</Repository>[[:space:]]*$|\1|p' "$template")
[ -n "$digest" ] || { echo "immutable Covalent release digest is missing from $template" >&2; exit 1; }
legacy_digest='sha256:8b8b96bdea7437fecf6d9c3297c248fd9de7eeb25fe7d701aa6f0a5b633cf8a6'

command -v ssh >/dev/null 2>&1 || { echo "ssh is required" >&2; exit 1; }
"$repo_root/scripts/validate-unraid-template.sh" "$template"
if grep -Eiq 'tailscale\.sock|docker\.sock' "$repo_root/packaging/docker/compose.yaml" "$template"; then
  echo "packaging must not mount a Docker or Tailscale socket" >&2
  exit 1
fi

printf '%s\n' "Atlas preflight: immutable release digest $digest"
# Sources are passed as positional arguments, never interpolated into remote
# shell text. StrictHostKeyChecking=yes deliberately refuses an unknown host.
set --
old_ifs=$IFS
IFS='
'
for source in $sources; do
  set -- "$@" "$source"
done
IFS=$old_ifs

ssh -o BatchMode=yes -o StrictHostKeyChecking=yes "$ssh_target" \
  sh -s -- "$digest" "$atlas_host" /mnt/user "$@" < "$remote_helper"

if [ "$digest" = "$legacy_digest" ]; then
  echo "Atlas preflight: infrastructure checks passed, but deployment remains blocked: this v0.1.0 digest predates KEK and trusted-claim support." >&2
  exit 2
fi
echo "Atlas preflight: passed (no remote mutation or deployment performed)"

#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "$#" in
  0) template="$repo_root/packaging/unraid/covalent.xml" ;;
  1) template=$1 ;;
  *) echo "usage: scripts/validate-unraid-template.sh [template.xml]" >&2; exit 64 ;;
esac
[ -f "$template" ] || { echo "Unraid template is missing: $template" >&2; exit 1; }

if command -v xmllint >/dev/null 2>&1; then
  xmllint --noout "$template"
elif command -v python3 >/dev/null 2>&1; then
  python3 - "$template" <<'PY'
import sys
import xml.etree.ElementTree as ET

ET.parse(sys.argv[1])
PY
else
  echo "no XML parser available (install xmllint or Python 3)" >&2
  exit 1
fi

for required in \
  '<Privileged>false</Privileged>' \
  'Target="/source"' \
  'Target="/boot-source"' \
  'Target="/restore"' \
  'Target="/config"' \
  'Target="/data"' \
  'Target="/run/secrets/covalent-kek"' \
  'Target="8443"' \
  'Target="COVALENT_HTTPS_HOST"' \
  'Target="COVALENT_KEY_ENCRYPTION_KEY_VERSION"' \
  'Default="/mnt/user/system/covalent-secrets/key-encryption-key"' \
  '--user=99:100' \
  'install -d -o 99 -g 100 -m 700' \
  'install -d -m 700 "$claim_parent"' \
  'install -m 600 /dev/null "$setup_code_file"' \
  'test ! -e "$claim_output"' \
  'claim output directory mode 0700' \
  'Android app never accept a setup code' \
  'never map /mnt/user/system' \
  'Mode="ro"' \
  'Default="false"'; do
  if ! grep -q -- "$required" "$template"; then
    echo "Unraid template missing required safe policy: $required" >&2
    exit 1
  fi
done

repository=$(sed -n 's|^[[:space:]]*<Repository>\(.*\)</Repository>[[:space:]]*$|\1|p' "$template")
if grep -q '<WebUI>http://' "$template" \
  || ! printf '%s\n' "$repository" | grep -Eq '^ghcr\.io/thekozugroup/covalent@sha256:[0-9a-f]{64}$'; then
  echo "Unraid template must use TLS management and an immutable Covalent GHCR digest" >&2
  exit 1
fi

icon_path="$repo_root/packaging/unraid/icon.svg"
if [ ! -s "$icon_path" ]; then
  echo "Unraid icon is missing" >&2
  exit 1
fi

if grep -q 'Target="/mnt/user"' "$template" || grep -q 'Target="/boot"' "$template"; then
  echo "Unraid template must not map broad host roots directly" >&2
  exit 1
fi

config_line() {
  target=$1
  matches=$(grep '<Config ' "$template" | grep -F "Target=\"$target\"" || true)
  count=$(printf '%s\n' "$matches" | grep -c . || true)
  if [ "$count" -ne 1 ]; then
    echo "Unraid template must contain exactly one mapping for $target" >&2
    exit 1
  fi
  printf '%s\n' "$matches"
}

require_mode() {
  target=$1
  mode=$2
  line=$(config_line "$target")
  if ! printf '%s\n' "$line" | grep -Fq "Mode=\"$mode\""; then
    echo "Unraid mapping $target must use mode $mode" >&2
    exit 1
  fi
}

# These are the intended deployment mounts. Generic occurrences of Mode=ro
# are insufficient: every input and the separate KEK must be read-only, while
# only state and the explicit restore destination may be writable.
require_mode /source ro
require_mode /boot-source ro
require_mode /run/secrets/covalent-kek ro
require_mode /config rw
require_mode /data rw
require_mode /restore rw
require_mode 8443 tcp
require_mode 8787 udp

if grep -Eiq '(docker|tailscale)\.sock|/var/run/docker|/var/run/tailscale' "$template"; then
  echo "Unraid template must not mount Docker or Tailscale control sockets" >&2
  exit 1
fi

extra_params=$(sed -n 's|^[[:space:]]*<ExtraParams>\(.*\)</ExtraParams>[[:space:]]*$|\1|p' "$template")
for required_param in '--user=99:100' '--read-only' '--cap-drop=ALL' '--security-opt=no-new-privileges'; do
  if ! printf '%s\n' "$extra_params" | grep -Fq -- "$required_param"; then
    echo "Unraid template missing container boundary: $required_param" >&2
    exit 1
  fi
done

if grep -q '/mnt/disks/covalent-secrets\|Unassigned Devices' "$template"; then
  echo "Unraid template must use the standard reserved system-share KEK path" >&2
  exit 1
fi

if grep -q 'Open the WebUI and enter that code' "$template" \
  || ! grep -q 'covalent claim --https-url' "$template" \
  || ! grep -q 'Do not put this KEK in /data or /config' "$template" \
  || ! grep -q 'fixed non-root UID/GID (`99:100`)' "$template"; then
  echo "Unraid template must document separate KEK provisioning and trusted token-only claiming" >&2
  exit 1
fi

echo "Unraid template XML: ok"

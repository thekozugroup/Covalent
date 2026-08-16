#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
template="$repo_root/packaging/unraid/covalent.xml"

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
  'Target="8443"' \
  'Target="COVALENT_HTTPS_HOST"' \
  'Mode="ro"' \
  'Default="false"'; do
  if ! grep -q "$required" "$template"; then
    echo "Unraid template missing required safe policy: $required" >&2
    exit 1
  fi
done

if grep -q '<WebUI>http://' "$template" || grep -q ':latest</Repository>' "$template"; then
  echo "Unraid template must use TLS management and a versioned image" >&2
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

echo "Unraid template XML: ok"

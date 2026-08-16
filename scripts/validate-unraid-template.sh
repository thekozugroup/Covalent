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

echo "Unraid template XML: ok"

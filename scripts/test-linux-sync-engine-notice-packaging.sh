#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
tmp_root=${TMPDIR:-/tmp}
case "$tmp_root" in /*) ;; *) tmp_root=/tmp ;; esac
fixture=$(mktemp -d "$tmp_root/covalent-linux-notice-package.XXXXXX")
cleanup() { chmod -R u+w "$fixture" 2>/dev/null || true; rm -rf "$fixture"; }
trap cleanup EXIT INT TERM

mkdir -p "$fixture/bin" "$fixture/worker" "$fixture/notices/modules/0000"
cat > "$fixture/bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) echo Linux ;;
esac
EOF
cat > "$fixture/bin/cc" <<EOF
#!/bin/sh
if [ "\${1:-}" = -print-prog-name=readelf ]; then
  echo "$fixture/bin/readelf"
  exit 0
fi
output=
while [ "\$#" -gt 0 ]; do
  if [ "\$1" = -o ]; then output=\$2; shift 2; else shift; fi
done
test -n "\$output"
dd if=/dev/zero of="\$output" bs=8192 count=1 status=none
chmod 0555 "\$output"
EOF
cat > "$fixture/bin/readelf" <<'EOF'
#!/bin/sh
case "$1" in
  -hW) echo '  Machine:                           Advanced Micro Devices X86-64' ;;
  -dW) echo 'There is no dynamic section in this file.' ;;
  -lW) echo 'Elf file type is DYN (Position-Independent Executable file)' ;;
  *) exit 64 ;;
esac
EOF
chmod 0555 "$fixture/bin/uname" "$fixture/bin/cc" "$fixture/bin/readelf"

dd if=/dev/zero of="$fixture/worker/covalent-syncthing" \
  bs=1048576 count=16 status=none
chmod 0555 "$fixture/worker/covalent-syncthing"
cp "$repo_root/packaging/docker/sync-engine-notices/Syncthing-LICENSE.txt" \
  "$fixture/worker/Syncthing-LICENSE.txt"
cp "$repo_root/packaging/docker/sync-engine-notices/Syncthing-AUTHORS.txt" \
  "$fixture/worker/Syncthing-AUTHORS.txt"
printf '{"ImportPath":"example"}\n' > "$fixture/worker/go-target-deps.ndjson"
printf 'path\tgithub.com/syncthing/syncthing/cmd/syncthing\nbuild\tgo1.26.7\n' \
  > "$fixture/worker/go-version.txt"
cat > "$fixture/worker/target-license-inventory.json" <<'EOF'
{
  "status": "evidence-collected-review-required",
  "targets": [{
    "buildTags": ["noupgrade"],
    "cgoEnabled": false,
    "goarch": "amd64",
    "goos": "linux"
  }]
}
EOF
inventory_sha=$(sha256sum "$fixture/worker/target-license-inventory.json" | awk '{print $1}')
printf '{"inventorySha256": "%s"}\n' "$inventory_sha" \
  > "$fixture/notices/manifest.json"
printf 'combined notice\n' > "$fixture/notices/THIRD-PARTY-NOTICES.txt"
printf 'module notice\n' > "$fixture/notices/modules/0000/LICENSE"

PATH="$fixture/bin:$PATH" "$repo_root/scripts/build-linux-sync-engine.sh" package \
  "$fixture/worker" \
  "$repo_root/packaging/sync-engine/engine-guardian.c" \
  "$fixture/notices" \
  "$fixture/package" \
  amd64

cmp "$fixture/worker/go-target-deps.ndjson" \
  "$fixture/package/share/evidence/go-target-deps.ndjson"
cmp "$fixture/worker/go-version.txt" \
  "$fixture/package/share/evidence/go-version.txt"
cmp "$fixture/worker/target-license-inventory.json" \
  "$fixture/package/share/evidence/target-license-inventory.json"
cmp "$fixture/notices/THIRD-PARTY-NOTICES.txt" \
  "$fixture/package/share/THIRD-PARTY-NOTICES.txt"
python3 - "$fixture/package/share/manifest.json" <<'PY'
import json, pathlib, sys
manifest = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert manifest["notices"]["thirdPartyStatus"] == "target-texts-collected-review-required"
assert manifest["targetEvidence"]["goListBytes"] > 0
assert manifest["targetEvidence"]["goVersionBytes"] > 0
assert manifest["targetEvidence"]["licenseInventoryBytes"] > 0
PY

mkdir "$fixture/notices-unsafe"
printf '{"inventorySha256": "%s"}\n' "$inventory_sha" \
  > "$fixture/notices-unsafe/manifest.json"
printf 'combined\n' > "$fixture/notices-unsafe/THIRD-PARTY-NOTICES.txt"
mkfifo "$fixture/notices-unsafe/unexpected"
if PATH="$fixture/bin:$PATH" "$repo_root/scripts/build-linux-sync-engine.sh" package \
    "$fixture/worker" \
    "$repo_root/packaging/sync-engine/engine-guardian.c" \
    "$fixture/notices-unsafe" \
    "$fixture/unsafe-package" \
    amd64 >/dev/null 2>&1; then
  echo "Linux engine packager accepted a special notice entry" >&2
  exit 1
fi
test ! -e "$fixture/unsafe-package"

echo "Linux sync-engine target notice packaging: ok"

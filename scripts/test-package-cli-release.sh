#!/bin/sh
# Regression fixture: a relative output directory remains valid after packaging
# changes into its private staging directory.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/covalent-cli-package.XXXXXX")
fixture=$(CDPATH= cd -- "$fixture" && pwd -P)
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT INT TERM

mkdir -p "$fixture/bin"
cat > "$fixture/covalent" <<'EOF'
#!/bin/sh
test "${1:-}" = --version
echo 'covalent 0.2.0'
EOF
chmod +x "$fixture/covalent"

cat > "$fixture/bin/file" <<'EOF'
#!/bin/sh
test "${1:-}" = -b
printf '%s\n' 'ELF 64-bit LSB pie executable, x86-64'
EOF
chmod +x "$fixture/bin/file"

(
  cd "$fixture"
  PATH="$fixture/bin:$PATH" "$repo_root/scripts/package-cli-release.sh" \
    --binary ./covalent --platform linux-amd64 --version v0.2.0 \
    --output-dir release-assets > package.log
)

archive="$fixture/release-assets/Covalent-v0.2.0-linux-amd64.tar.gz"
test -s "$archive"
tar -tzf "$archive" | grep -Fxq 'Covalent-v0.2.0-linux-amd64/covalent'
grep -Fqx "archive=$archive" "$fixture/package.log"

echo "relative CLI package output fixture: ok"

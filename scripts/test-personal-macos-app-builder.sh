#!/bin/sh
# Fast contract test for the personal macOS packaging entry point. The real
# bundle verifier remains the authority for an actual built app.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
builder="$repo_root/scripts/build-personal-macos-app.sh"
fixture=$(mktemp -d "${TMPDIR:-/tmp}/covalent-personal-builder-contract.XXXXXX")
trap 'rm -rf -- "$fixture"' EXIT HUP INT TERM

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

test -x "$builder" || fail "personal macOS builder is not executable"
sh -n "$builder"
git -C "$repo_root" check-ignore -q artifacts/install/.ignore-check ||
  fail "artifacts/install must stay ignored"

# These are literal source contracts, not expressions to expand in this test.
# shellcheck disable=SC2016
for contract in \
  '"$(uname -s)" != Darwin' \
  '"$(uname -m)" != arm64' \
  'scripts/install-xcodegen.sh' \
  'scripts/setup-doctor.sh' \
  'COVALENT_GO_ARCHIVE' \
  '020a1e8224811be75163e920bc77e0926a1390a6aeea19bdcf23f74b9d749f6d' \
  'scripts/release-version.sh' \
  'scripts/docker-source-fingerprint.sh' \
  'xcodegen generate --quiet' \
  '-disableAutomaticPackageResolution' \
  '-onlyUsePackageVersionsFromResolvedFile' \
  'ARCHS=arm64' \
  'EXCLUDED_ARCHS=x86_64' \
  'CODE_SIGN_IDENTITY=-' \
  'scripts/verify-apple-silicon-bundle.sh' \
  'Signature=adhoc' \
  'artifacts/install' \
  'shasum -a 256 -c' \
  'ditto -x -k' \
  'build-receipt.json' \
  '"sourceCommit": commit' \
  '"sourceFingerprint": fingerprint' \
  '"covalent-node": descriptor(node)' \
  '"covalent-rclone": descriptor(worker)' \
  '"covalent-engine-guardian": descriptor(guardian)' \
  '"engineManifest": descriptor(manifest)' \
  'mv -n'
do
  grep -Fq -- "$contract" "$builder" || fail "missing builder contract: $contract"
done

BUILDER="$builder" OUTPUT="$fixture/write-receipt.py" PYTHONDONTWRITEBYTECODE=1 python3 - <<'PYEXTRACT'
import os
from pathlib import Path
text=Path(os.environ["BUILDER"]).read_text()
start=text.index("<<'PY'\n",text.index('python3 - "$staged_receipt"'))+len("<<'PY'\n")
end=text.index("\nPY\n",start)
body=text[start:end]
compile(body,os.environ["BUILDER"]+":receipt-python","exec")
Path(os.environ["OUTPUT"]).write_text(body)
PYEXTRACT
python3 - "$fixture" <<'PYFIXTURE'
import hashlib,json,pathlib,subprocess,sys
root=pathlib.Path(sys.argv[1])
paths=[root/name for name in ("archive","app","node","worker","guardian","manifest")]
for index,path in enumerate(paths): path.write_bytes((f"fixture {index}\n").encode())
receipt=root/"build-receipt.json"
subprocess.run([sys.executable,str(root/"write-receipt.py"),str(receipt),"a"*40,"b"*64,"0.2.0",*(str(path) for path in paths)],check=True)
value=json.loads(receipt.read_text())
assert value["schemaVersion"]==1 and value["sourceCommit"]=="a"*40
assert value["sourceFingerprint"]=="b"*64 and value["releaseVersion"]=="0.2.0"
assert value["archive"]["sha256"]==hashlib.sha256(paths[0].read_bytes()).hexdigest()
assert value["components"]["covalent-node"]["sha256"]==hashlib.sha256(paths[2].read_bytes()).hexdigest()
PYFIXTURE

if grep -Eq 'notarytool|altool|stapler|security[[:space:]]+import|open[[:space:]]+-[Ra]' "$builder" ||
  grep -Eq '(^|[[:space:]])(cp|mv|ditto)[[:space:]].*/Applications(/|[[:space:]]|$)' "$builder"; then
  fail "personal builder must not sign for distribution, notarize, or install"
fi

# shellcheck disable=SC2016
grep -Fq 'if [ -e "$path" ] || [ -L "$path" ]; then' "$builder" ||
  fail "builder must refuse existing files and symlinks"
grep -Fq './scripts/build-personal-macos-app.sh' "$repo_root/docs/platform/macos.md" ||
  fail "macOS guide must use the one-command builder"
grep -Fq './scripts/build-personal-macos-app.sh' "$repo_root/apps/apple/README.md" ||
  fail "Apple developer README must use the one-command builder"

echo "Personal macOS builder contract: ok"

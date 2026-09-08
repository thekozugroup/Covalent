#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/covalent-android-package.XXXXXX")
cleanup() {
  rm -rf "$fixture"
}
trap cleanup EXIT INT TERM

cat > "$fixture/zipalign" <<'SH'
#!/bin/sh
exit 0
SH
chmod 700 "$fixture/zipalign"

python3 - "$fixture" <<'PY'
import hashlib
import pathlib
import warnings
import zipfile

root = pathlib.Path(__import__("sys").argv[1])
abis = ("arm64-v8a", "x86_64")
payloads = {
    "libcovalent_android_jni.so": b"j" * (4 * 1024 * 1024),
    "libsyncthing.so": b"s" * (16 * 1024 * 1024),
    "libengineguardian.so": b"g" * (8 * 1024),
}

def make(name, bad_hash=False, omit_guardian=False, duplicate_worker=False):
    manifests = {}
    for library, payload in payloads.items():
        rows = []
        for abi in abis:
            digest = hashlib.sha256(payload).hexdigest()
            if bad_hash and library == "libsyncthing.so" and abi == "arm64-v8a":
                digest = "0" * 64
            rows.append(f"{abi} {digest} {len(payload)}")
        manifests[library] = "\n".join(rows) + "\n"
    with zipfile.ZipFile(root / name, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for abi in abis:
            for library, payload in payloads.items():
                if omit_guardian and library == "libengineguardian.so" and abi == "x86_64":
                    continue
                archive.writestr(f"lib/{abi}/{library}", payload)
        archive.writestr("assets/syncthing-sha256.txt", manifests["libsyncthing.so"])
        archive.writestr("assets/engine-guardian-sha256.txt", manifests["libengineguardian.so"])
        if duplicate_worker:
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                archive.writestr("lib/arm64-v8a/libsyncthing.so", payloads["libsyncthing.so"])

make("valid.apk")
make("bad-hash.apk", bad_hash=True)
make("missing.apk", omit_guardian=True)
make("duplicate.apk", duplicate_worker=True)
PY

export COVALENT_ANDROID_ZIPALIGN="$fixture/zipalign"
"$repo_root/scripts/check-android-native-package.sh" "$fixture/valid.apk" >/dev/null

for rejected in bad-hash missing duplicate; do
  if "$repo_root/scripts/check-android-native-package.sh" \
    "$fixture/$rejected.apk" >"$fixture/$rejected.out" 2>&1; then
    echo "Android package gate accepted the $rejected fixture" >&2
    exit 1
  fi
done

echo "Android native package regression fixtures passed."

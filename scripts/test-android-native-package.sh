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
import json
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

def make(name, bad_hash=False, omit_guardian=False, duplicate_worker=False, bad_notice=False):
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
        ndk = b"fixture Android NDK notice\n"
        toolchain = b"fixture LLVM toolchain notice\n"
        combined = (
            b"Covalent synchronized-folder engine notices\n"
            b"\n===== Android NDK 27.1.12297006 / NOTICE =====\n" + ndk
            + b"\n===== Android NDK 27.1.12297006 / NOTICE.toolchain =====\n" + toolchain
        )
        toolchain_rows = []
        for source_name, label, data in (
            ("NOTICE", "Android NDK 27.1.12297006 / NOTICE", ndk),
            ("NOTICE.toolchain", "Android NDK 27.1.12297006 / NOTICE.toolchain", toolchain),
        ):
            path = f"sync-engine-notices/toolchain/{source_name}"
            archive.writestr("assets/" + path, data)
            toolchain_rows.append({
                "label": label,
                "sourceName": source_name,
                "bundlePath": f"toolchain/{source_name}",
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
            })
        if bad_notice:
            toolchain_rows[0]["sha256"] = "0" * 64
        notice_manifest = json.dumps({
            "schemaVersion": 1,
            "status": "texts-collected-review-required",
            "combinedNotice": {
                "bytes": len(combined),
                "sha256": hashlib.sha256(combined).hexdigest(),
            },
            "toolchainNotices": toolchain_rows,
        }, sort_keys=True).encode()
        archive.writestr("assets/sync-engine-notices/THIRD-PARTY-NOTICES.txt", combined)
        archive.writestr("assets/sync-engine-notices/manifest.json", notice_manifest)
        archive.writestr(
            "assets/sync-engine-notices-index.txt",
            "1\n"
            f"combined {hashlib.sha256(combined).hexdigest()} {len(combined)} sync-engine-notices/THIRD-PARTY-NOTICES.txt\n"
            f"manifest {hashlib.sha256(notice_manifest).hexdigest()} {len(notice_manifest)} sync-engine-notices/manifest.json\n",
        )
        if duplicate_worker:
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                archive.writestr("lib/arm64-v8a/libsyncthing.so", payloads["libsyncthing.so"])

make("valid.apk")
make("bad-hash.apk", bad_hash=True)
make("missing.apk", omit_guardian=True)
make("duplicate.apk", duplicate_worker=True)
make("bad-notice.apk", bad_notice=True)
PY

export COVALENT_ANDROID_ZIPALIGN="$fixture/zipalign"
"$repo_root/scripts/check-android-native-package.sh" "$fixture/valid.apk" >/dev/null

for rejected in bad-hash missing duplicate bad-notice; do
  if "$repo_root/scripts/check-android-native-package.sh" \
    "$fixture/$rejected.apk" >"$fixture/$rejected.out" 2>&1; then
    echo "Android package gate accepted the $rejected fixture" >&2
    exit 1
  fi
done

echo "Android native package regression fixtures passed."

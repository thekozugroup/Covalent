#!/usr/bin/env python3
"""Verify/sign the dedicated APK; production identity is supplied, never generated.

Only INTERLINK_ANDROID_* key variables are accepted. Nothing falls back to the
Covalent signer or Android's debug keystore. Passwords stay in the environment.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile

APPLICATION_ID = "life.michaelwong.interlink"
ROOT = Path(__file__).resolve().parent.parent
KEY_VARIABLES = ("INTERLINK_ANDROID_KEYSTORE_PATH", "INTERLINK_ANDROID_STORE_PASSWORD",
                 "INTERLINK_ANDROID_KEY_ALIAS", "INTERLINK_ANDROID_KEY_PASSWORD")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(command, *, input=None, check=True, credentials=False, extra_env=None):
    # Tool diagnostics can contain key paths or sensitive provider messages.
    # Return a short fixed failure rather than echoing their output/arguments.
    environment = dict(os.environ)
    for prefix in ("COVALENT_ANDROID_", "INTERLINK_ANDROID_"):
        for suffix in ("STORE_PASSWORD", "KEY_PASSWORD"):
            if prefix == "INTERLINK_ANDROID_" and credentials:
                continue
            environment.pop(prefix + suffix, None)
    environment.update(extra_env or {})
    result = subprocess.run(command, input=input, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=180, env=environment)
    if check:
        require(result.returncode == 0, f"{Path(command[0]).name} verification failed")
    return result


def fingerprint(value):
    require(re.fullmatch(r"[0-9a-fA-F]{64}", value or "") is not None,
            "A pinned 64-digit signing certificate SHA-256 is required")
    return value.lower()


def inspect_apk(apk, tools, version, version_code, *, certificate=None):
    require(apk.is_file() and 0 < apk.stat().st_size <= 96 * 1024 * 1024, "APK is missing or exceeds its size bound")
    with zipfile.ZipFile(apk) as archive:
        names = archive.namelist()
        require(len(names) == len(set(names)), "APK has duplicate archive entries")
        require(sum(entry.file_size for entry in archive.infolist()) <= 384 * 1024 * 1024
                and all(entry.file_size <= 96 * 1024 * 1024 for entry in archive.infolist()),
                "APK expanded contents exceed their byte bound")
        require("AndroidManifest.xml" in names and archive.testzip() is None, "APK archive is corrupt")
    badging = run([str(tools / "aapt2"), "dump", "badging", str(apk)]).stdout.decode("utf-8")
    package = re.search(r"^package: name='([^']+)' versionCode='([^']+)' versionName='([^']+)'", badging, re.M)
    require(package is not None and package.groups() == (APPLICATION_ID, str(version_code), version),
            "APK package or version differs from the requested Interlink release")
    require("application-debuggable" not in badging, "Production APK must not be debuggable")
    run([str(tools / "zipalign"), "-c", "-P", "16", "4", str(apk)])
    run([str(ROOT / "scripts/check-android-native-package.sh"), str(apk)],
        extra_env={"COVALENT_ANDROID_ZIPALIGN": str(tools / "zipalign")})
    result = run([str(tools / "apksigner"), "verify", "--verbose", "--print-certs", str(apk)], check=False)
    if certificate is None:
        require(result.returncode != 0, "Signing input must be an unsigned release APK")
    else:
        require(result.returncode == 0, "Signed APK signature verification failed")
        evidence = result.stdout.decode("utf-8")
        # Build-tools 37 labels its signer by v3 version instead of signer index.
        # Verify every reported certificate; never confuse a key digest with it
        # or accept an extra/unknown signer while searching for the pinned row.
        counts = re.findall(r"^Number of signers: (\d+)$", evidence, re.M)
        rows = [line for line in evidence.splitlines() if "certificate SHA-256 digest:" in line]
        parsed = [re.fullmatch(r"(Signer #1|V3\.[012] Signer:) certificate SHA-256 digest: ([0-9a-fA-F]{64})", line)
                  for line in rows]
        require(counts == ["1"] and parsed and all(parsed)
                and len({row[1] for row in parsed}) == len(parsed)
                and all(row[2].lower() == fingerprint(certificate) for row in parsed),
                "APK signer differs from the pinned Interlink identity")
        require("android debug" not in evidence.lower(), "Android Debug signing is forbidden for a production release")
        for scheme in (2, 3):
            require(re.search(rf"^Verified using v{scheme} scheme .*: true$", evidence, re.M) is not None,
                    f"APK must verify with signature scheme v{scheme}")
    return {"applicationId": APPLICATION_ID, "versionName": version, "versionCode": version_code,
            "apkSha256": sha256(apk), "bytes": apk.stat().st_size,
            "certificateSha256": certificate}


def sign(args):
    expected = fingerprint(os.environ.get("INTERLINK_ANDROID_CERTIFICATE_SHA256"))
    require(re.fullmatch(r"[0-9a-f]{40}", args.source_revision) is not None, "Exact 40-digit source revision required")
    require(all(os.environ.get(name, "").strip() for name in KEY_VARIABLES),
            "All four INTERLINK_ANDROID signing inputs are required; there is no fallback signer")
    key = Path(os.environ[KEY_VARIABLES[0]])
    require(key.is_file(), "Interlink keystore is missing")
    require(stat.S_IMODE(key.stat().st_mode) & 0o077 == 0, "Interlink keystore must be owner-only")
    certificate = run(["keytool", "-exportcert", "-keystore", str(key),
        "-alias", os.environ[KEY_VARIABLES[2]], "-storepass:env", KEY_VARIABLES[1]], credentials=True).stdout
    require(hashlib.sha256(certificate).hexdigest() == expected, "Keystore certificate differs from the pinned Interlink identity")
    description = run(["keytool", "-J-Duser.language=en", "-printcert"], input=certificate).stdout.decode("utf-8")
    require("android debug" not in description.lower(), "Android Debug signing is forbidden for a production release")
    output = args.output_directory.absolute()
    require(not output.exists(), "Output directory already exists; signed artifacts are never overwritten")
    output.parent.mkdir(parents=True, exist_ok=True)
    # Copy the input once, verify that snapshot, then sign it. A changing build
    # directory cannot replace the already-verified bytes during credential use.
    with tempfile.TemporaryDirectory(prefix=".interlink-sign-", dir=output.parent) as work:
        work = Path(work)
        unsigned = work / "unsigned.apk"
        shutil.copyfile(args.unsigned_apk, unsigned)
        before = inspect_apk(unsigned, args.build_tools, args.version, args.version_code)
        signed = work / f"Interlink-v{args.version}-android.apk"
        run([str(args.build_tools / "apksigner"), "sign", "--ks", str(key),
             "--ks-key-alias", os.environ[KEY_VARIABLES[2]],
             "--ks-pass", "env:" + KEY_VARIABLES[1], "--key-pass", "env:" + KEY_VARIABLES[3],
             "--v1-signing-enabled", "true", "--v2-signing-enabled", "true",
             "--v3-signing-enabled", "true", "--v4-signing-enabled", "false",
             "--out", str(signed), str(unsigned)], credentials=True)
        receipt = inspect_apk(signed, args.build_tools, args.version, args.version_code, certificate=expected)
        require(sha256(unsigned) == before["apkSha256"], "Unsigned APK changed during signing")
        receipt.update(schemaVersion=1, sourceRevision=args.source_revision,
                       unsignedApkSha256=before["apkSha256"], signingPolicy="pinned-production-identity")
        # Only public release material reaches the result directory.
        ready = work / "ready"
        ready.mkdir()
        shutil.move(signed, ready / signed.name)
        (ready / "release-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        files = sorted(ready.iterdir())
        (ready / "SHA256SUMS").write_text("".join(f"{sha256(p)}  {p.name}\n" for p in files))
        # mkdir is exclusive even if another signing process raced our earlier
        # existence check. Never replace a previously produced release directory.
        output.mkdir()
        for path in ready.iterdir():
            shutil.move(path, output / path.name)
    print(json.dumps(receipt))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ("verify", "sign"):
        item = sub.add_parser(command)
        item.add_argument("--build-tools", type=Path, required=True)
        item.add_argument("--version", required=True)
        item.add_argument("--version-code", type=int, required=True)
        if command == "verify":
            item.add_argument("--apk", type=Path, required=True)
            item.add_argument("--certificate-sha256")
        else:
            item.add_argument("--unsigned-apk", type=Path, required=True)
            item.add_argument("--output-directory", type=Path, required=True)
            item.add_argument("--source-revision", required=True)
    args = parser.parse_args()
    require(re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", args.version) is not None, "Explicit semantic version required")
    require(args.version_code > 0, "Positive Android version code required")
    require(args.build_tools.is_absolute() and all((args.build_tools / n).is_file()
        for n in ("apksigner", "zipalign", "aapt2")), "Explicit Android build-tools directory required")
    if args.command == "sign":
        sign(args)
    else:
        cert = fingerprint(args.certificate_sha256) if args.certificate_sha256 else None
        print(json.dumps(inspect_apk(args.apk, args.build_tools, args.version, args.version_code, certificate=cert)))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, zipfile.BadZipFile, subprocess.TimeoutExpired) as error:
        # No raw tool output or environment values are printed.
        print(f"Interlink release refused: {error}", file=sys.stderr)
        sys.exit(1)

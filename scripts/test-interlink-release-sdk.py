#!/usr/bin/env python3
"""Real SDK signing smoke with disposable test keys. Never installs/publishes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--build-tools", type=Path, required=True)
parser.add_argument("--unsigned-apk", type=Path, required=True)
parser.add_argument("--version", required=True)
parser.add_argument("--version-code", type=int, required=True)
args = parser.parse_args()


def run(command, env):
    return subprocess.run(command, env=env, capture_output=True, text=False, timeout=180)


with tempfile.TemporaryDirectory(prefix="interlink-signing-smoke-") as temp:
    root = Path(temp)
    env = dict(os.environ)
    env.update(INTERLINK_ANDROID_STORE_PASSWORD="disposable-test-password",
               INTERLINK_ANDROID_KEY_PASSWORD="disposable-test-password", INTERLINK_ANDROID_KEY_ALIAS="test-only")
    evidence = []
    for name, subject in (("test-only", "Interlink Release Guard Test Only"), ("debug", "Android Debug")):
        key = root / f"{name}.p12"
        env["INTERLINK_ANDROID_KEYSTORE_PATH"] = str(key)
        command = ["keytool", "-genkeypair", "-keystore", str(key), "-storetype", "PKCS12",
            "-alias", "test-only", "-keyalg", "RSA", "-keysize", "2048", "-validity", "1",
            "-dname", f"CN={subject}", "-storepass:env", "INTERLINK_ANDROID_STORE_PASSWORD",
            "-keypass:env", "INTERLINK_ANDROID_KEY_PASSWORD"]
        assert run(command, env).returncode == 0, "Could not generate disposable smoke-test key"
        key.chmod(0o600)
        certificate = run(["keytool", "-exportcert", "-keystore", str(key), "-alias", "test-only",
            "-storepass:env", "INTERLINK_ANDROID_STORE_PASSWORD"], env)
        assert certificate.returncode == 0
        env["INTERLINK_ANDROID_CERTIFICATE_SHA256"] = hashlib.sha256(certificate.stdout).hexdigest()
        output = root / (name + "-result")
        result = run(["python3", "-B", str(ROOT / "scripts/interlink-release.py"), "sign",
            "--build-tools", str(args.build_tools), "--unsigned-apk", str(args.unsigned_apk.resolve()),
            "--version", args.version, "--version-code", str(args.version_code),
            "--source-revision", "0" * 40, "--output-directory", str(output)], env)
        if name == "debug":
            assert result.returncode != 0 and b"Android Debug" in result.stderr and not output.exists(), "Debug-key guard failed"
            evidence.append("matching pinned Android Debug key rejected")
        else:
            assert result.returncode == 0, result.stderr.decode()
            receipt = json.loads(result.stdout)
            assert receipt["certificateSha256"] == env["INTERLINK_ANDROID_CERTIFICATE_SHA256"]
            assert receipt["unsignedApkSha256"] == hashlib.sha256(args.unsigned_apk.read_bytes()).hexdigest()
            assert not list(output.glob("*.p12"))
            original = (output / "release-receipt.json").read_bytes()
            again = run(["python3", "-B", str(ROOT / "scripts/interlink-release.py"), "sign",
                "--build-tools", str(args.build_tools), "--unsigned-apk", str(args.unsigned_apk.resolve()),
                "--version", args.version, "--version-code", str(args.version_code),
                "--source-revision", "0" * 40, "--output-directory", str(output)], env)
            assert again.returncode != 0 and (output / "release-receipt.json").read_bytes() == original
            evidence += ["real APK v2/v3 signatures and pinned certificate verified", "existing output preserved"]
    print(json.dumps({"scope": "disposable-test-keys-only-no-install-or-publication", "checks": evidence}))

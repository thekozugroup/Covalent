#!/usr/bin/env python3
"""Credential/signature-policy regressions. Real SDK smoke is a separate gate."""
import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location("interlink_release", Path(__file__).with_name("interlink-release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)
CERT = b"synthetic test certificate, never a production identity"
PIN = hashlib.sha256(CERT).hexdigest()


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.apk = self.root / "unsigned.apk"
        with zipfile.ZipFile(self.apk, "w") as archive:
            archive.writestr("AndroidManifest.xml", b"synthetic manifest")
        self.key = self.root / "key.jks"
        self.key.write_bytes(b"synthetic key")
        self.key.chmod(0o600)
        self.args = types.SimpleNamespace(build_tools=self.root, version="0.1.0-dev", version_code=1,
            source_revision="a" * 40, unsigned_apk=self.apk, output_directory=self.root / "result")
        self.env = dict(zip(release.KEY_VARIABLES, (str(self.key), "private-store-password", "interlink", "private-key-password")))
        self.env["INTERLINK_ANDROID_CERTIFICATE_SHA256"] = PIN
        self.debug = False
        self.badging = "package: name='life.michaelwong.interlink' versionCode='1' versionName='0.1.0-dev'\n"
        self.signed_cert = PIN
        self.signer_label = "Signer #1"
        self.signer_count = "Number of signers: 1\n"
        self.extra_certificates = ""
        self.bad_scheme = False
        self.commands = []

    def tool(self, command, *, input=None, check=True, credentials=False, extra_env=None):
        self.commands.append(command)
        name = Path(command[0]).name
        code, output = 0, b""
        if name == "keytool":
            output = CERT if "-exportcert" in command else (b"Owner: CN=Android Debug" if self.debug else b"Owner: CN=Interlink Test")
        elif name == "aapt2":
            output = self.badging.encode()
        elif name == "apksigner":
            if command[1] == "sign":
                Path(command[command.index("--out") + 1]).write_bytes(Path(command[-1]).read_bytes())
            elif Path(command[-1]).name == "unsigned.apk":
                code = 1
            else:
                output = (self.signer_count + f"{self.signer_label} certificate SHA-256 digest: {self.signed_cert}\n"
                    + self.extra_certificates +
                    "Signer #1 certificate DN: CN=Interlink Test\n"
                    "Verified using v2 scheme (APK Signature Scheme v2): true\n"
                    f"Verified using v3 scheme (APK Signature Scheme v3): {str(not self.bad_scheme).lower()}\n").encode()
        return subprocess.CompletedProcess(command, code, output, b"")

    def sign(self):
        with patch.dict(os.environ, self.env, clear=True), patch.object(release, "run", self.tool), contextlib.redirect_stdout(io.StringIO()):
            release.sign(self.args)

    def test_signs_only_pinned_identity_and_emits_no_credentials(self):
        self.sign()
        files = {p.name for p in self.args.output_directory.iterdir()}
        self.assertEqual(files, {"Interlink-v0.1.0-dev-android.apk", "release-receipt.json", "SHA256SUMS"})
        public = b"".join(p.read_bytes() for p in self.args.output_directory.iterdir())
        for secret in ("private-store-password", "private-key-password", str(self.key)):
            self.assertNotIn(secret.encode(), public)
        for secret in ("private-store-password", "private-key-password"):
            self.assertNotIn(secret, str(self.commands))

    def test_covalent_credentials_do_not_substitute_for_interlink(self):
        self.env = {name.replace("INTERLINK_", "COVALENT_"): value for name, value in self.env.items()}
        with self.assertRaisesRegex(ValueError, "pinned"):
            self.sign()

    def test_missing_key_input_refuses(self):
        del self.env[release.KEY_VARIABLES[-1]]
        with self.assertRaisesRegex(ValueError, "four INTERLINK"):
            self.sign()

    def test_wrong_key_pin_refuses_before_signing(self):
        self.env["INTERLINK_ANDROID_CERTIFICATE_SHA256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "Keystore certificate"):
            self.sign()
        self.assertFalse(any(c[1] == "sign" for c in self.commands))

    def test_debug_key_refuses_even_with_matching_pin(self):
        self.debug = True
        with self.assertRaisesRegex(ValueError, "Android Debug"):
            self.sign()

    def test_shared_readable_keystore_refuses(self):
        self.key.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "owner-only"):
            self.sign()

    def test_wrong_application_refuses(self):
        self.badging = self.badging.replace("interlink", "covalent")
        with self.assertRaisesRegex(ValueError, "package or version"):
            self.sign()

    def test_wrong_version_refuses(self):
        self.args.version_code = 2
        with self.assertRaisesRegex(ValueError, "package or version"):
            self.sign()

    def test_debuggable_package_refuses(self):
        self.badging += "application-debuggable\n"
        with self.assertRaisesRegex(ValueError, "debuggable"):
            self.sign()

    def test_post_sign_certificate_mismatch_refuses(self):
        self.signed_cert = "0" * 64
        with self.assertRaisesRegex(ValueError, "APK signer"):
            self.sign()
        self.assertFalse(self.args.output_directory.exists())

    def test_sdk37_versioned_signer_label_accepts_exact_pin(self):
        self.signer_label = "V3.0 Signer:"
        self.sign()

    def test_sdk37_additional_version_must_use_same_identity(self):
        self.signer_label = "V3.0 Signer:"
        self.extra_certificates = f"V3.1 Signer: certificate SHA-256 digest: {PIN}\n"
        self.sign()

    def test_sdk37_different_versioned_certificate_refuses(self):
        self.signer_label = "V3.0 Signer:"
        self.extra_certificates = f"V3.1 Signer: certificate SHA-256 digest: {'0' * 64}\n"
        with self.assertRaisesRegex(ValueError, "APK signer"):
            self.sign()
        self.assertFalse(self.args.output_directory.exists())

    def test_extra_unknown_or_duplicate_certificate_refuses(self):
        for label in ("Signer #2", "V3.3 Signer:", "Signer #1"):
            with self.subTest(label=label):
                self.extra_certificates = f"{label} certificate SHA-256 digest: {PIN}\n"
                with self.assertRaisesRegex(ValueError, "APK signer"):
                    self.sign()
                self.assertFalse(self.args.output_directory.exists())

    def test_missing_or_multiple_signer_count_refuses(self):
        for count in ("", "Number of signers: 2\n", "Number of signers: 1\nNumber of signers: 1\n"):
            with self.subTest(count=count):
                self.signer_count = count
                with self.assertRaisesRegex(ValueError, "APK signer"):
                    self.sign()
                self.assertFalse(self.args.output_directory.exists())

    def test_uppercase_certificate_digest_matches_same_bytes(self):
        self.signed_cert = PIN.upper()
        self.sign()

    def test_missing_v3_signature_refuses(self):
        self.bad_scheme = True
        with self.assertRaisesRegex(ValueError, "v3"):
            self.sign()

    def test_signed_output_is_not_overwritten(self):
        self.args.output_directory.mkdir()
        original = self.args.output_directory / "keep"
        original.write_bytes(b"retain")
        with self.assertRaisesRegex(ValueError, "already exists"):
            self.sign()
        self.assertEqual(original.read_bytes(), b"retain")

    def test_duplicate_archive_entry_refuses(self):
        import warnings
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            with zipfile.ZipFile(self.apk, "a") as archive:
                archive.writestr("AndroidManifest.xml", b"duplicate")
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.sign()

    def test_tool_failures_never_echo_secret_diagnostics(self):
        result = subprocess.CompletedProcess(["tool"], 1, b"private-store-password", b"private-key-password")
        with patch.object(subprocess, "run", return_value=result), self.assertRaisesRegex(ValueError, "tool verification failed") as error:
            release.run(["tool", "sensitive-argument"])
        self.assertNotIn("private-", str(error.exception))

    def test_only_signing_tools_receive_key_passwords(self):
        result = subprocess.CompletedProcess(["tool"], 0, b"", b"")
        with patch.dict(os.environ, self.env, clear=True), patch.object(subprocess, "run", return_value=result) as call:
            release.run(["tool"])
            self.assertNotIn("INTERLINK_ANDROID_STORE_PASSWORD", call.call_args.kwargs["env"])
            release.run(["keytool"], credentials=True)
            self.assertEqual(call.call_args.kwargs["env"]["INTERLINK_ANDROID_STORE_PASSWORD"], "private-store-password")


if __name__ == "__main__":
    unittest.main()

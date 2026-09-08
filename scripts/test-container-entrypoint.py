#!/usr/bin/env python3
"""Exercise lifecycle dispatch using only private local stub processes."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

ENTRYPOINT = Path(__file__).resolve().parents[1] / "packaging/docker/entrypoint.sh"


class EntrypointTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="covalent-entrypoint-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for name in ("bin", "config", "data"):
            (self.root / name).mkdir()
        (self.root / "kek").write_text("disposable stub fixture")
        for name in ("covalent-node", "caddy"):
            executable = self.root / "bin" / name
            executable.write_text(
                '#!/bin/sh\nset -eu\n'
                # Existence is the parent's readiness signal. Publish the
                # complete argument vector atomically, after printf closes it.
                'printf "%s\\n" "$@" > "$FIXTURE_ROOT/' + name + '.args.tmp"\n'
                'mv "$FIXTURE_ROOT/' + name + '.args.tmp" "$FIXTURE_ROOT/' + name + '.args"\n'
                'case "${1:-}" in provision-key) exit 0 ;; esac\n'
                'exec sleep 30\n'
            )
            executable.chmod(0o700)
        self.env = {
            **os.environ,
            "PATH": str(self.root / "bin") + os.pathsep + os.environ["PATH"],
            "FIXTURE_ROOT": str(self.root),
            "COVALENT_CONFIG_DIR": str(self.root / "config"),
            "COVALENT_DATA_DIR": str(self.root / "data"),
            "COVALENT_KEY_ENCRYPTION_KEY_FILE": str(self.root / "kek"),
            "XDG_DATA_HOME": str(self.root / "config/caddy/data"),
            "XDG_CONFIG_HOME": str(self.root / "config/caddy/config"),
            "UMASK": "027",
        }
        for name in ("PUID", "PGID", "COVALENT_HTTPS_HOST", "COVALENT_ADVERTISED_PEER_ADDRESS"):
            self.env.pop(name, None)

    def start(self, *args):
        process = subprocess.Popen(["sh", str(ENTRYPOINT), *args], env=self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True)
        self.addCleanup(self.stop, process)
        return process

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=5)
        else:
            process.communicate(timeout=5)

    def assert_supervised(self, command, *args):
        process = self.start(command, *args)
        for _ in range(200):
            if (self.root / "caddy.args").exists() and (self.root / "covalent-node.args").exists():
                break
            self.assertIsNone(process.poll(), "entrypoint exited before starting both children")
            time.sleep(0.01)
        else:
            self.fail("both supervised children did not start")
        self.assertEqual((self.root / "covalent-node.args").read_text().splitlines(), [command, *args])
        self.assertEqual((self.root / "caddy.args").read_text().splitlines(),
                         ["run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"])
        process.send_signal(signal.SIGTERM)
        _, error = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 0, error.decode())

    def test_normal_start_is_supervised(self):
        self.assert_supervised("serve")

    def test_recovery_retains_tls_proxy_and_exact_file_arguments(self):
        self.assert_supervised("recover", "--recovery-kit-file", "/run/secrets/saved kit",
                               "--recovery-key-file", "/run/secrets/code")

    def test_provisioning_does_not_start_proxy_or_require_mounts(self):
        self.env["COVALENT_CONFIG_DIR"] = str(self.root / "missing")
        process = self.start("provision-key", "--key-file", "/secrets/new-key")
        process.communicate(timeout=5)
        self.assertEqual(process.returncode, 0)
        self.assertFalse((self.root / "caddy.args").exists())
        self.assertFalse((self.root / "config/caddy").exists())

    def test_recovery_without_separate_kek_fails_before_start(self):
        (self.root / "kek").unlink()
        process = self.start("recover")
        process.communicate(timeout=5)
        self.assertEqual(process.returncode, 78)
        self.assertFalse((self.root / "covalent-node.args").exists())
        self.assertFalse((self.root / "caddy.args").exists())


if __name__ == "__main__":
    unittest.main()

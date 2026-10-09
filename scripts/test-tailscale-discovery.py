#!/usr/bin/env python3
"""Exercise host snapshot publication with a stub CLI and the real jq parser."""
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parent / "refresh-tailscale-discovery.sh"


class DiscoveryRefreshTests(unittest.TestCase):
    def test_snapshot_is_sanitized_atomic_and_preserves_expiry_on_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "config with spaces"
            config.mkdir()
            status = root / "status.json"
            status.write_text(json.dumps({
                "BackendState": "Running", "Secret": "must not leave host",
                "Peer": {
                    "nodekey:one": {"Online": True, "DNSName": "atlas.tail.test.",
                                    "TailscaleIPs": ["100.64.0.2", "fd7a:115c:a1e0::2", "8.8.8.8"],
                                    "UserID": 123, "PublicKey": "must not leave host"},
                    "nodekey:two": {"Online": False, "DNSName": "offline.tail.test.",
                                    "TailscaleIPs": ["100.64.0.3"]},
                    "nodekey:three": {"Online": True, "DNSName": "invalid.tail.test.",
                                      "TailscaleIPs": ["192.168.1.2", "100.63.0.1", "100.64.999.1"]},
                },
            }))
            cli = root / "tailscale"
            cli.write_text('#!/bin/sh\n[ "$*" = "status --json" ] || exit 64\ncat "$FIXTURE_STATUS"\n')
            cli.chmod(0o700)
            if not shutil.which('timeout'):
                timeout = root / 'timeout'
                timeout.write_text(f'#!{sys.executable}\nimport subprocess, sys\nsys.exit(subprocess.run(sys.argv[2:], timeout=float(sys.argv[1])).returncode)\n')
                timeout.chmod(0o700)
            env = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                   "FIXTURE_STATUS": str(status)}

            def refresh():
                return subprocess.run(["sh", str(SCRIPT), str(config)], env=env,
                                      capture_output=True, text=True, timeout=10)

            result = refresh()
            self.assertEqual(result.returncode, 0, result.stderr)
            output = config / "tailscale-discovery.json"
            original = output.read_bytes()
            snapshot = json.loads(original)
            self.assertEqual(snapshot["schemaVersion"], 1)
            self.assertLess(abs(snapshot["generatedAtUnixMs"] - int(time.time() * 1000)), 10000)
            self.assertEqual(snapshot["status"], {"Peer": {"0": {
                "DNSName": "atlas.tail.test.",
                "TailscaleIPs": ["100.64.0.2", "fd7a:115c:a1e0::2"],
            }}})
            self.assertEqual(output.stat().st_mode & 0o777, 0o644)

            for invalid in ("not JSON", '{"BackendState":"Stopped","Peer":{}}'):
                status.write_text(invalid)
                self.assertNotEqual(refresh().returncode, 0)
                self.assertEqual(output.read_bytes(), original)
                self.assertEqual(list(config.iterdir()), [output])

            status.write_text(json.dumps({"BackendState": "Running", "Peer": {}}))
            self.assertEqual(refresh().returncode, 0)
            self.assertEqual(json.loads(output.read_bytes())["status"], {"Peer": {}})


if __name__ == "__main__":
    unittest.main()

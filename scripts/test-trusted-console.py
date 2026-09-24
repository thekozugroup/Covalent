#!/usr/bin/env python3
"""Check trusted console configuration and, with --image, its actual proxy boundary."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("configure", ROOT / "scripts/configure-trusted-console.py")
configure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(configure)
TOKEN = "disposable-test-token-" + "a" * 32
ORIGIN = "https://console.example.ts.net:8443"
LOGIN = "owner@example.com"


def main():
    with tempfile.TemporaryDirectory(prefix="covalent-console-test-") as temporary:
        directory = Path(temporary)
        token = directory / "token"
        token.write_text(TOKEN)
        token.chmod(0o600)
        configure.configure(directory, token, ORIGIN, LOGIN)
        rule = directory / "console-access/tailscale.caddy"
        assert rule.stat().st_mode & 0o777 == 0o600
        assert rule.parent.stat().st_mode & 0o777 == 0o700
        for origin, login, value in [("http://console.example", LOGIN, TOKEN),
                                      (ORIGIN + "/path", LOGIN, TOKEN),
                                      (ORIGIN, "*", TOKEN),
                                      (ORIGIN, LOGIN, TOKEN + '"\n}')]:
            try:
                configure.access_rule(origin, login, value)
            except ValueError:
                pass
            else:
                raise AssertionError("unsafe input accepted")
        token.chmod(0o644)
        try:
            configure.configure(directory, token, ORIGIN, LOGIN)
        except ValueError:
            pass
        else:
            raise AssertionError("world-readable token accepted")
        print("Trusted console configuration checks passed")
        if len(sys.argv) == 1:
            return
        assert sys.argv[1] == "--image" and len(sys.argv) == 3
        runtime_check(directory, sys.argv[2])


def runtime_check(directory, image):
    # Run the shipped Caddy config with a disposable token-checking backend.
    # Only a random loopback port, temporary mounts, and disposable credentials.
    config = (ROOT / "packaging/docker/Caddyfile").read_text()
    config += f'''\nhttp://:8787 {{
    bind 127.0.0.1
    @authorized header Authorization "Bearer {TOKEN}"
    respond @authorized "authorized" 200
    respond "authentication required" 401
}}\n'''
    (directory / "Caddyfile").write_text(config)
    name = f"covalent-console-test-{os.getpid()}"

    def docker(*args, check=True):
        result = subprocess.run(["docker", *args], capture_output=True, text=True)
        if check and result.returncode:
            raise RuntimeError(result.stderr[-2000:])
        return result

    docker("run", "-d", "--name", name, "--publish", "127.0.0.1::8443", "--user", f"{os.getuid()}:{os.getgid()}",
           "--cap-drop=ALL", "--security-opt=no-new-privileges", "--entrypoint", "caddy",
           "-v", f"{directory}:/config", image, "run", "--config", "/config/Caddyfile", "--adapter", "caddyfile")
    try:
        for _ in range(60):
            if docker("inspect", name, "--format", "{{.State.Running}}").stdout.strip() != "true":
                log = docker("logs", name)
                raise RuntimeError((log.stdout + log.stderr)[-3000:])
            if docker("exec", name, "test", "-S", "/config/covalent-web.sock", check=False).returncode == 0:
                break
            time.sleep(0.2)
        else:
            log = docker("logs", name)
            raise RuntimeError((log.stdout + log.stderr)[-3000:])
        good = {"Host": "console.example.ts.net:8443", "Tailscale-User-Login": LOGIN,
                "X-Covalent-Console": "1", "Sec-Fetch-Site": "same-origin", "Origin": ORIGIN}
        port = docker("port", name, "8443/tcp").stdout.strip().rsplit(":", 1)[1]

        def request(headers, expected, direct=False):
            args = ["curl", "-sS", "-w", "\n%{http_code}", "-X", "POST"]
            if direct:
                args += ["--cacert", str(directory / "caddy/data/caddy/pki/authorities/local/root.crt"),
                         "--resolve", f"localhost:{port}:127.0.0.1",
                         f"https://localhost:{port}/api/v1/config/export"]
                headers = {**headers, "Host": f"localhost:{port}"}
            else:
                args += ["--unix-socket", str(directory / "covalent-web.sock"), "http://localhost/api/v1/config/export"]
            for key, value in headers.items():
                args += ["-H", f"{key}: {value}"]
            response = subprocess.run(args, capture_output=True, text=True, check=True)
            body, status = response.stdout.rsplit("\n", 1)
            assert status == str(expected), f"expected {expected}, got {status} for {list(headers)}"
            assert body == ("authorized" if expected == 200 else "authentication required")

        request(good, 200)
        request({**good, "Host": "localhost"}, 200)
        request({k: v for k, v in good.items() if k != "Origin"}, 200)
        for key in ("Tailscale-User-Login", "X-Covalent-Console", "Sec-Fetch-Site"):
            request({k: v for k, v in good.items() if k != key}, 401)
        for key, value in [("Host", "evil.example"), ("Tailscale-User-Login", "other@example.com"),
                           ("Origin", "https://evil.example"), ("Origin", "null"),
                           ("Sec-Fetch-Site", "same-site"), ("Sec-Fetch-Site", "cross-site")]:
            request({**good, key: value}, 401)
        request(good, 401, direct=True)
        request({"Authorization": f"Bearer {TOKEN}"}, 200, direct=True)
        print("Proxy boundary passed: trusted access, missing/foreign identity, CSRF, direct spoofing, token fallback")
    finally:
        docker("rm", "-f", name, check=False)


if __name__ == "__main__":
    main()

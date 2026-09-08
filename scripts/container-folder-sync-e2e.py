#!/usr/bin/env python3
"""Exercise two packaged nodes using exclusively owned, bounded Docker resources.

No host folders, fixed ports, pre-existing containers, or existing volumes are
used. Bearer tokens go through stdin into private read-only secret volumes.
"""
from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
import secrets
import selectors
import signal
import socket
import ssl
import subprocess
import time
import uuid


class GateError(Exception):
    pass


def docker(*args: str, data: bytes | None = None, timeout: int = 45) -> bytes:
    # All stdin payloads are small fixture/token writes and fit in an atomic
    # pipe write. Drain both output streams with an aggregate bound while the
    # process is running; do not first materialize unbounded Docker output.
    if data is not None and len(data) > 4096:
        raise GateError("Docker fixture input exceeds its bound")
    process = None
    try:
        process = subprocess.Popen(["docker", *args], stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True)
        if data is not None:
            process.stdin.write(data)
        process.stdin.close()
        deadline = time.monotonic() + timeout
        output = bytearray()
        retained = 0
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ, True)
            selector.register(process.stderr, selectors.EVENT_READ, False)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise GateError("bounded Docker command did not complete")
                for key, _ in selector.select(min(0.25, remaining)):
                    chunk = os.read(key.fileobj.fileno(), 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    retained += len(chunk)
                    if retained > 2 * 1024 * 1024:
                        raise GateError("Docker command exceeded its response bound")
                    if key.data:
                        output.extend(chunk)
        code = process.wait(timeout=max(0.001, deadline - time.monotonic()))
        if code:
            raise GateError("Docker command failed")
        return bytes(output)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise GateError("bounded Docker command did not complete") from error
    finally:
        if process is not None:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            for pipe in (process.stdin, process.stdout, process.stderr):
                pipe.close()


class LocalTls(http.client.HTTPSConnection):
    """Dial the published loopback port while verifying the actual server name."""
    def connect(self):
        raw = socket.create_connection(("127.0.0.1", self.port), self.timeout)
        try:
            self.sock = self._context.wrap_socket(raw, server_hostname=self.host)
        except BaseException:
            raw.close()
            raise


class Fixture:
    def __init__(self, image: str):
        self.image = docker("image", "inspect", image, "--format", "{{.Id}}").decode("ascii").strip()
        self.prefix = "covalent-sync-gate-" + uuid.uuid4().hex[:16]
        self.network = self.prefix + "-network"
        self.network_created = False
        self.volumes: list[str] = []
        self.containers: list[str] = []
        self.nodes: dict[str, dict] = {}
        self.checks: list[str] = []
        self.phase = "fixture creation"
        self.diagnostics: dict = {}

    def setup(self):
        self.phase = "isolated network creation"
        self.network_created = True
        docker("network", "create", "--label", "covalent.sync-gate=" + self.prefix, self.network)
        for key in ("a", "b"):
            self.phase = "isolated volume creation for node " + key
            name = self.prefix + "-" + key
            volumes = {}
            for kind in ("config", "data", "sync", "secrets"):
                volume = name + "-" + kind
                self.volumes.append(volume)
                docker("volume", "create", "--label", "covalent.sync-gate=" + self.prefix, volume)
                volumes[kind] = volume
            token = secrets.token_hex(32)
            mounts = []
            for kind, volume in volumes.items():
                mounts += ["--mount", f"type=volume,source={volume},target=/{kind}"]
            initializer = name + "-initialize"
            self.containers.append(initializer)
            self.phase = "private secret initialization for node " + key
            docker("run", "--rm", "--name", initializer,
                   "--label", "covalent.sync-gate=" + self.prefix,
                   "--interactive", "--network=none", "--read-only",
                   "--cpus=0.5", "--memory=128m", "--pids-limit=64",
                   "--cap-drop=ALL", "--cap-add=CHOWN", "--cap-add=FOWNER",
                   "--security-opt=no-new-privileges", "--user=0:0", *mounts,
                   "--entrypoint=sh", self.image, "-c",
                   "set -eu; umask 077; IFS= read -r token; printf '%s' \"$token\" > /secrets/token; "
                   "covalent-node provision-key --key-file /secrets/kek >/dev/null; "
                   "chmod 600 /secrets/token /secrets/kek; "
                   "chown 65532:65532 /secrets/token /secrets/kek; "
                   "chmod 700 /config /data /sync /secrets; "
                   "chown 65532:65532 /config /data /sync /secrets",
                   data=(token + "\n").encode())
            self.containers.append(name)
            self.phase = "packaged node startup for node " + key
            docker("run", "--detach", "--name", name,
                   "--label", "covalent.sync-gate=" + self.prefix,
                   "--network", self.network, "--network-alias", name,
                   "--read-only", "--init", "--user=65532:65532",
                   "--cpus=0.75", "--memory=512m", "--pids-limit=256",
                   "--cap-drop=ALL", "--security-opt=no-new-privileges",
                   "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=64m,mode=1777",
                   "--publish", "127.0.0.1::8443/tcp",
                   *sum((["--mount", f"type=volume,source={volumes[kind]},target=/{kind}"]
                         for kind in ("config", "data", "sync")), []),
                   "--mount", f"type=volume,source={volumes['secrets']},target=/run/secrets,readonly",
                   "--env", "COVALENT_KEY_ENCRYPTION_KEY_FILE=/run/secrets/kek",
                   "--env", "COVALENT_HTTPS_HOST=" + name,
                   "--env", "COVALENT_DEVICE_NAME=Folder gate " + key,
                   "--env", "COVALENT_LAN_DISCOVERY=false",
                   self.image, "serve", "--api-token-file", "/run/secrets/token")
            self.nodes[key] = {"name": name, "token": token}
            self.refresh_endpoint(key)
            self.phase = "TLS readiness for node " + key
            self.wait(lambda: self.load_ca(key), "server CA creation")
            self.wait(lambda: self.request(key, "/api/v1/status").get("state") == "ready", "node readiness")
        self.checks.append("two-rootless-read-only-bounded-packaged-nodes")

    def refresh_endpoint(self, key: str):
        """Re-read Docker's current loopback mapping after every container start."""
        node = self.nodes[key]
        inspected = json.loads(docker("inspect", node["name"]))[0]
        if (inspected["Config"]["Labels"].get("covalent.sync-gate") != self.prefix
                or inspected["Name"].removeprefix("/") != node["name"]
                or not inspected["State"]["Running"]):
            raise GateError("the owned container is not running")
        bindings = inspected["NetworkSettings"]["Ports"]["8443/tcp"]
        if len(bindings) != 1 or bindings[0]["HostIp"] != "127.0.0.1":
            raise GateError("the test API must have one loopback-only mapping")
        port = int(bindings[0]["HostPort"])
        if not 1 <= port <= 65535:
            raise GateError("the test API mapping has an invalid port")
        node["port"] = port
        node["ip"] = inspected["NetworkSettings"]["Networks"][self.network]["IPAddress"]

    def load_ca(self, key: str) -> bool:
        node = self.nodes[key]
        certificate = docker("exec", node["name"], "cat", "/config/caddy/data/caddy/pki/authorities/local/root.crt")
        node["tls"] = ssl.create_default_context(cadata=certificate.decode("ascii"))
        return True

    def request(self, key: str, path: str, payload: dict | None = None):
        node = self.nodes[key]
        client = LocalTls(node["name"], node["port"], context=node["tls"], timeout=15)
        try:
            client.request("GET" if payload is None else "POST", path,
                           body=None if payload is None else json.dumps(payload),
                           headers={"Authorization": "Bearer " + node["token"],
                                    "Accept": "application/json", "Content-Type": "application/json"})
            response = client.getresponse()
            raw = response.read(2 * 1024 * 1024 + 1)
            if not 200 <= response.status < 300 or len(raw) > 2 * 1024 * 1024:
                raise GateError("folder API rejected the bounded test request")
            return json.loads(raw)
        finally:
            client.close()

    @staticmethod
    def wait(predicate, label: str, seconds: int = 90):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                if predicate():
                    return
            except (GateError, OSError, http.client.HTTPException, ValueError):
                pass
            time.sleep(0.25)
        raise GateError(label + " did not converge before its deadline")

    def write(self, key: str, filename: str, payload: bytes):
        docker("exec", "--interactive", self.nodes[key]["name"], "sh", "-c",
               "umask 077; cat > \"$1\"", "sh", "/sync/" + filename, data=payload)

    def matches(self, key: str, filename: str, payload: bytes) -> bool:
        actual = docker("exec", self.nodes[key]["name"], "sha256sum", "/sync/" + filename)
        return actual.decode("ascii").split()[0] == hashlib.sha256(payload).hexdigest()

    def status(self, key: str):
        return self.request(key, "/api/v1/sync/status")

    def capture_diagnostics(self):
        """Retain only bounded public state, never tokens, paths or peer data."""
        enums = {
            "availability": {"notPackaged", "needsAttention", "available"},
            "lifecycle": {"stopped", "initialScanning", "running", "stillStopping", "needsAttention"},
            "issue": {None, "installation", "folderAccess", "journal", "workerLaunch", "initialScan",
                      "workerHealth", "workerStop", "peerRevocation"},
            "healthFreshness": {"neverObserved", "fresh", "stale"},
            "connectionFreshness": {"neverObserved", "fresh", "stale"},
        }
        for key in ("a", "b"):
            if key not in self.nodes:
                continue
            try:
                status = self.status(key)
                row = {name: value for name, allowed in enums.items()
                       if isinstance(value := status.get(name), (str, type(None))) and value in allowed}
                row["shares"] = [
                    {name: value for name, allowed in {
                        "phase": {"offered", "awaitingCommit", "ready", "paused", "removed"},
                        "peerConnection": {"unknown", "connected", "disconnected", "paused"},
                    }.items() if isinstance(value := share.get(name), str) and value in allowed}
                    for share in status.get("shares", [])[:8] if isinstance(share, dict)
                ]
                row["folders"] = [
                    {name: value for name in ("remainingFiles", "remainingBytes", "scanPullErrorCount", "reportedErrorRows")
                     if type(value := folder.get(name)) is int and 0 <= value <= 2**63 - 1}
                    | {name: value for name in ("statusError", "watchError")
                       if type(value := folder.get(name)) is bool}
                    | ({"state": folder["state"]} if folder.get("state") in {
                        "starting", "idle", "scanning", "scan-waiting", "sync-waiting", "sync-preparing",
                        "syncing", "cleaning", "clean-waiting", "error"} else {})
                    for folder in status.get("folders", [])[:8] if isinstance(folder, dict)
                ]
                self.diagnostics[key] = row
            except (GateError, OSError, ValueError, TypeError, http.client.HTTPException):
                self.diagnostics[key] = {"status": "unavailable"}

    def exercise(self):
        self.phase = "mutual signed pairing"
        invitation = self.request("a", "/api/v1/pair/invitations",
                                  {"lifetimeMs": 600000, "endpoints": [self.nodes["a"]["ip"] + ":8787"]})
        session = self.request("b", "/api/v1/pair/accept", {
            "invitation": invitation, "responderName": "Folder gate b",
            "responderRoles": ["storage_provider", "backup_reader"],
            "inviterRoles": ["backup_writer", "backup_reader"],
        })
        for key, role in (("b", "responder"), ("a", "inviter")):
            session = self.request(key, "/api/v1/pair/confirm/" + role,
                                   {"session": session, "displayedCode": session["authenticationString"]})
        self.request("a", "/api/v1/pair/finalize/inviter", {"session": session})
        self.request("b", "/api/v1/pair/finalize/responder", {"session": session})
        peer = self.request("b", "/api/v1/transport/identity")["deviceId"]
        self.checks.append("mutual-signed-pairing")
        self.phase = "durable signed folder offer"
        payload = b"packaged-container-folder-gate\n"
        self.write("a", "forward.txt", payload)
        body = {"peerId": peer, "folderId": str(uuid.uuid4()), "label": "Container folder gate", "selectedRoot": "/sync"}
        offered = self.request("a", "/api/v1/sync/folders", body)
        offer_id = offered["offerId"]
        if self.request("a", "/api/v1/sync/folders", body)["offerId"] != offer_id:
            raise GateError("offer retry created a duplicate")
        self.wait(lambda: any(s["offerId"] == offer_id and s["incoming"] for s in self.status("b")["shares"]), "offer delivery")
        self.phase = "folder acceptance and forward transfer"
        self.request("b", "/api/v1/sync/accept", {"offerId": offer_id, "selectedRoot": "/sync"})
        self.wait(lambda: self.matches("b", "forward.txt", payload), "forward transfer")
        self.checks.append("idempotent-offer-dual-consent-full-scan-and-forward-transfer")
        self.phase = "pause and resume"
        self.request("a", "/api/v1/sync/pause", {"offerId": offer_id, "paused": True})
        self.wait(lambda: self.status("a")["lifecycle"] == "stopped", "paused worker stop")
        paused_payload = b"edit withheld while source paused\n"
        self.write("a", "forward.txt", paused_payload)
        deadline = time.monotonic() + 7
        while time.monotonic() < deadline:
            if not self.matches("b", "forward.txt", payload):
                raise GateError("paused source exchanged a file")
            time.sleep(0.25)
        self.request("a", "/api/v1/sync/pause", {"offerId": offer_id, "paused": False})
        self.wait(lambda: self.matches("b", "forward.txt", paused_payload), "resumed transfer")
        self.checks.append("pause-withholds-edit-and-resume-converges")
        self.phase = "cold restart and reverse transfer"
        before = self.request("b", "/api/v1/transport/identity")
        docker("stop", "--time=20", self.nodes["b"]["name"])
        docker("start", self.nodes["b"]["name"])
        # Docker can allocate another host port for the original ephemeral
        # binding. Preserve the original CA/server identity while using the
        # currently assigned mapping to verify the restarted node.
        self.refresh_endpoint("b")
        self.wait(lambda: self.request("b", "/api/v1/status").get("state") == "ready", "cold restart")
        if self.request("b", "/api/v1/transport/identity") != before:
            raise GateError("cold restart replaced identity")
        reverse = b"reverse edit after packaged node restart\n"
        self.write("b", "reverse.txt", reverse)
        self.wait(lambda: self.matches("a", "reverse.txt", reverse), "reverse transfer after restart")
        self.checks.append("durable-identity-restart-and-reverse-transfer")
        self.phase = "offline peer removal and file preservation"
        # Pause only this fixture's recipient. Its network identity and mounted
        # files remain allocated while it cannot acknowledge a control request.
        docker("pause", self.nodes["b"]["name"])
        self.request("a", "/api/v1/sync/remove", {"offerId": offer_id})
        if not self.matches("a", "forward.txt", paused_payload) or not self.matches("a", "reverse.txt", reverse):
            raise GateError("local removal changed selected files")
        self.wait(lambda: self.status("a")["lifecycle"] == "stopped", "removed worker stop")
        if not self.matches("a", "forward.txt", paused_payload) or not self.matches("a", "reverse.txt", reverse):
            raise GateError("completed local removal changed selected files")
        self.checks.append("local-removal-preserves-files-and-stops-worker")

        def removed(key: str, pending: bool) -> bool:
            status = self.status(key)
            rows = [row for row in status["shares"] if row["offerId"] == offer_id]
            return (len(rows) == 1 and rows[0]["phase"] == "removed"
                    and rows[0].get("remoteRemovalPending") is pending
                    and status["lifecycle"] == "stopped")

        self.wait(lambda: removed("a", True), "durable offline removal")
        self.phase = "pending removal survives sender cold restart"
        sender_identity = self.request("a", "/api/v1/transport/identity")
        docker("stop", "--time=20", self.nodes["a"]["name"])
        docker("start", self.nodes["a"]["name"])
        self.refresh_endpoint("a")
        self.wait(lambda: self.request("a", "/api/v1/status").get("state") == "ready", "sender cold restart")
        if self.request("a", "/api/v1/transport/identity") != sender_identity:
            raise GateError("sender cold restart replaced identity")
        self.wait(lambda: removed("a", True), "cold pending removal")
        self.checks.append("offline-removal-outbox-survives-sender-cold-restart")

        self.phase = "authenticated removal delivery and acknowledgement"
        docker("unpause", self.nodes["b"]["name"])
        self.wait(lambda: removed("b", False), "signed removal delivery without echo")
        self.wait(lambda: removed("a", False), "durable authenticated removal acknowledgement")
        docker("stop", "--time=20", self.nodes["b"]["name"])
        docker("start", self.nodes["b"]["name"])
        self.refresh_endpoint("b")
        self.wait(lambda: self.request("b", "/api/v1/status").get("state") == "ready", "removed recipient cold restart")
        if self.request("b", "/api/v1/transport/identity") != before:
            raise GateError("removed recipient cold restart replaced identity")
        self.wait(lambda: removed("b", False), "cold recipient removal")
        for key in ("a", "b"):
            if not self.matches(key, "forward.txt", paused_payload) or not self.matches(key, "reverse.txt", reverse):
                raise GateError("remote removal changed an existing file")
        self.checks.append("signed-remote-removal-acknowledgement-and-recipient-cold-restart-keep-both-copies")

    @staticmethod
    def exists(kind: str, name: str) -> bool:
        if kind == "container":
            names = docker("ps", "--all", "--filter", "name=" + name, "--format", "{{.Names}}")
        else:
            names = docker(kind, "ls", "--filter", "name=" + name, "--format", "{{.Name}}")
        return name in names.decode("utf-8").splitlines()

    def cleanup(self):
        failures = 0
        forced = 0
        for name in reversed(self.containers):
            try:
                if not self.exists("container", name):
                    continue
                row = json.loads(docker("inspect", name))[0]
                if row["Config"]["Labels"].get("covalent.sync-gate") != self.prefix:
                    raise GateError("container ownership differs")
                try:
                    if row["State"].get("Paused"):
                        docker("unpause", name)
                    docker("stop", "--time=20", name)
                    if self.exists("container", name):
                        stopped = json.loads(docker("inspect", name))[0]["State"]
                        if stopped["Running"] or stopped["OOMKilled"] or stopped["ExitCode"] != 0:
                            forced += 1
                except GateError:
                    forced += 1
                # A timed-out Docker client does not prove the daemon stopped
                # its container. Remove the exact labelled object independently.
                if self.exists("container", name):
                    docker("rm", "--force", name)
            except GateError:
                failures += 1
        for volume in reversed(self.volumes):
            try:
                if not self.exists("volume", volume):
                    continue
                row = json.loads(docker("volume", "inspect", volume))[0]
                if row["Labels"].get("covalent.sync-gate") != self.prefix:
                    raise GateError("volume ownership differs")
                docker("volume", "rm", volume)
            except GateError:
                failures += 1
        if self.network_created:
            try:
                if self.exists("network", self.network):
                    row = json.loads(docker("network", "inspect", self.network))[0]
                    if row["Labels"].get("covalent.sync-gate") != self.prefix:
                        raise GateError("network ownership differs")
                    docker("network", "rm", self.network)
            except GateError:
                failures += 1
        if failures:
            raise GateError("one or more test-owned Docker resources require cleanup: " + self.prefix)
        self.checks.append("all-owned-containers-volumes-and-network-removed")
        if forced:
            raise GateError("owned resources were removed, but a container did not stop cleanly")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", help="An already built, exact Covalent image")
    arguments = parser.parse_args()
    def interrupted(_signal, _frame):
        raise GateError("test interrupted; cleaning owned resources")
    signal.signal(signal.SIGTERM, interrupted)
    fixture = Fixture(arguments.image)
    try:
        try:
            fixture.setup()
            fixture.exercise()
        except (GateError, OSError, ValueError, http.client.HTTPException):
            fixture.capture_diagnostics()
            raise
        finally:
            fixture.cleanup()
    except (GateError, OSError, ValueError, http.client.HTTPException) as error:
        print(json.dumps({"status": "failed", "phase": fixture.phase,
                          "reason": str(error), "passedChecks": fixture.checks,
                          "nodeStates": fixture.diagnostics}))
        return 1
    print(json.dumps({"status": "passed", "checks": fixture.checks}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

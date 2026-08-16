#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"
image="${1:-covalent:e2e}"
compose="docker compose -f packaging/docker/compose.e2e.yaml"
source_dir="packaging/docker/example/source"
restore_a="packaging/docker/example/restore-a"
restore_b="packaging/docker/example/restore-b"
restore_c="packaging/docker/example/restore-c"

cleanup() {
  $compose down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$source_dir/nested" "$restore_a/nested" "$restore_b/nested" "$restore_c/nested"
}
trap cleanup EXIT INT TERM

mkdir -p "$source_dir/nested" "$restore_a" "$restore_b" "$restore_c"
printf 'container-disaster-recovery\n' > "$source_dir/nested/payload.txt"
printf 'empty fixture\n' > "$source_dir/nested/empty.txt"

COVALENT_IMAGE="$image" $compose up -d --wait

token() { $compose exec -T "$1" sh -c 'cat /data/local-api-token'; }
token_a=$(token node-a)
token_b=$(token node-b)
token_c=$(token node-c)
node_b_ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$($compose ps -q node-b)")
node_c_ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$($compose ps -q node-c)")

TOKEN_A="$token_a" TOKEN_B="$token_b" TOKEN_C="$token_c" NODE_B_IP="$node_b_ip" NODE_C_IP="$node_c_ip" python3 - <<'PY'
import json
import os
import time
import urllib.error
import urllib.request

ports = {"a": 18781, "b": 18782, "c": 18783}
tokens = {"a": os.environ["TOKEN_A"], "b": os.environ["TOKEN_B"], "c": os.environ["TOKEN_C"]}

def request(node, path, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    headers = {"Accept": "application/json", "Authorization": "Bearer " + tokens[node]}
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(f"http://127.0.0.1:{ports[node]}{path}", data=data, headers=headers, method="POST" if payload is not None else "GET")
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            body = response.read()
            return None if not body else json.loads(body)
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"{node} {path}: {error.read().decode()}") from error

for _ in range(30):
    try:
        if all(request(node, "/api/v1/status")["state"] == "ready" for node in ports):
            break
    except (OSError, urllib.error.URLError):
        time.sleep(1)
else:
    raise SystemExit("all compose nodes did not become ready")

def pair(inviter, responder, responder_name):
    invitation = request(inviter, "/api/v1/pair/invitations", {"lifetimeMs": 600000, "endpoints": [f"node-{inviter}:8787"]})
    session = request(responder, "/api/v1/pair/accept", {
        "invitation": invitation, "responderName": responder_name,
        "responderRoles": ["storage_provider", "backup_reader"],
        "inviterRoles": ["backup_writer", "backup_reader"],
    })
    session = request(responder, "/api/v1/pair/confirm/responder", {"session": session, "displayedCode": session["authenticationString"]})
    session = request(inviter, "/api/v1/pair/confirm/inviter", {"session": session, "displayedCode": session["authenticationString"]})
    request(inviter, "/api/v1/pair/finalize/inviter", {"session": session})
    request(responder, "/api/v1/pair/finalize/responder", {"session": session})

pair("a", "b", "Node B")
pair("a", "c", "Node C")
identity_b = request("b", "/api/v1/transport/identity")
identity_c = request("c", "/api/v1/transport/identity")
for identity, address in ((identity_b, os.environ["NODE_B_IP"]), (identity_c, os.environ["NODE_C_IP"])):
    request("a", "/api/v1/providers/connect", {"peerId": identity["deviceId"], "address": f"{address}:8787", "certificateDer": identity["certificateDer"]})

backup = request("a", "/api/v1/backups", {
    "sourceRoot": "/source", "displayName": "Compose disaster drill", "snapshotId": "compose-snapshot", "jobId": "compose-backup",
    "selectedProviderIds": [identity_b["deviceId"], identity_c["deviceId"]],
})
if backup["selectedProviders"] != 2 or backup["degradedFailures"] != 0:
    raise SystemExit("explicit two-provider replication did not complete")
availability = request("a", "/api/v1/backups/verify", {"backupId": backup["backupId"], "snapshotId": "compose-snapshot", "verifyProviders": True})
if not availability["intact"] or len(availability["providerAvailability"]) != 2:
    raise SystemExit("provider availability is incomplete")

settings = request("a", "/api/v1/config/export", {})
request("a", "/api/v1/config/import", {"confirmed": True, "settings": settings})
print(json.dumps({"backupId": backup["backupId"], "providerIds": [identity_b["deviceId"], identity_c["deviceId"]]}))
PY

# Simulate source loss and one local corrupt ciphertext. Repair must use an
# already paired, explicitly selected provider rather than the vanished source.
rm -rf "$source_dir/nested"
$compose exec -T node-a sh -c 'chunk=$(find /data/store/chunks -type f | head -n 1); test -n "$chunk"; printf corrupt > "$chunk"'

# Resolve the immutable backup through the same authenticated listing contract
# used by native and web clients.
backup_id=$(TOKEN_A="$token_a" python3 - <<'PY'
import json
import os
import urllib.request

request = urllib.request.Request(
    "http://127.0.0.1:18781/api/v1/backups",
    headers={"Accept": "application/json", "Authorization": "Bearer " + os.environ["TOKEN_A"]},
)
with urllib.request.urlopen(request, timeout=15) as response:
    backups = json.load(response)
matches = [backup for backup in backups if backup["latestSnapshotId"] == "compose-snapshot"]
if len(matches) != 1:
    raise SystemExit("backup listing did not return the compose snapshot exactly once")
print(matches[0]["backupId"])
PY
)
TOKEN_A="$token_a" BACKUP_ID="$backup_id" python3 - <<'PY'
import json
import os
import urllib.request

headers = {"Accept": "application/json", "Authorization": "Bearer " + os.environ["TOKEN_A"], "Content-Type": "application/json"}
def post(path, payload):
    request = urllib.request.Request("http://127.0.0.1:18781" + path, data=json.dumps(payload).encode(), headers=headers, method="POST")
    with urllib.request.urlopen(request, timeout=30) as response:
        return None if response.status == 204 else json.loads(response.read() or b"null")

request = {"backupId": os.environ["BACKUP_ID"], "snapshotId": "compose-snapshot"}
before = post("/api/v1/backups/verify", request)
if before["intact"]:
    raise SystemExit("corrupt local chunk was not rejected")
after = post("/api/v1/backups/verify", {**request, "repair": True})
if not after["intact"]:
    raise SystemExit("repair from paired provider failed")
plan = post("/api/v1/restores/preview", {**request, "targetRoot": "/restore", "conflictPolicy": "fail", "jobId": "compose-restore"})
if any(entry["destinationPath"].startswith("/") or ".." in entry["destinationPath"].split("/") for entry in plan["entries"]):
    raise SystemExit("restore preview escaped its authorized root")
result = post("/api/v1/restores/execute", {"plan": plan})
if result["filesRestored"] < 2:
    raise SystemExit("restore after source loss did not restore expected files")
print("compose E2E: pair, explicit replication, source loss, corruption rejection/repair, safe settings import, and root-confined restore: ok")
PY

$compose exec -T node-a test -f /restore/nested/payload.txt
echo "Docker Compose multi-node E2E: ok"

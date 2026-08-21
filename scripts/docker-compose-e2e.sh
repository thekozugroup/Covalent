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
tls_directory=$(mktemp -d)

cleanup() {
  $compose down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$source_dir/nested" "$restore_a/nested" "$restore_b/nested" "$restore_c/nested"
  rm -rf "$tls_directory"
}
trap cleanup EXIT INT TERM

mkdir -p "$source_dir/nested" "$restore_a" "$restore_b" "$restore_c"
printf 'container-disaster-recovery\n' > "$source_dir/nested/payload.txt"
printf 'empty fixture\n' > "$source_dir/nested/empty.txt"

COVALENT_IMAGE="$image" $compose up -d --wait

for service in node-a node-b node-c; do
  attempt=0
  until $compose exec -T "$service" test -f /config/caddy/data/caddy/pki/authorities/local/root.crt; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 20 ]; then
      $compose logs "$service" >&2
      exit 1
    fi
    sleep 1
  done
done
$compose cp node-a:/config/caddy/data/caddy/pki/authorities/local/root.crt "$tls_directory/a.crt" >/dev/null
$compose cp node-b:/config/caddy/data/caddy/pki/authorities/local/root.crt "$tls_directory/b.crt" >/dev/null
$compose cp node-c:/config/caddy/data/caddy/pki/authorities/local/root.crt "$tls_directory/c.crt" >/dev/null

token() { $compose exec -T "$1" sh -c 'cat /data/local-api-token'; }
token_a=$(token node-a)
token_b=$(token node-b)
token_c=$(token node-c)
# Read back rather than restate the addresses the compose file pinned. If the
# static assignment ever silently stopped taking effect, the node would still be
# advertising the pinned address while these read the real one, and the
# invitation below would be rejected as `pairing_endpoint_mismatch` instead of
# pairing three nodes that cannot actually reach each other.
peer_ip() { docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$($compose ps -q "$1")"; }
node_a_ip=$(peer_ip node-a)
node_b_ip=$(peer_ip node-b)
node_c_ip=$(peer_ip node-c)

if [ "${COVALENT_RUN_APPLE_TLS_E2E:-false}" = "true" ]; then
  "$repo_root/scripts/apple-package-tls-e2e.sh" \
    "https://127.0.0.1:18781" \
    "$tls_directory/a.crt" \
    "$token_a" \
    "$tls_directory/b.crt"
fi

TOKEN_A="$token_a" TOKEN_B="$token_b" TOKEN_C="$token_c" NODE_A_IP="$node_a_ip" NODE_B_IP="$node_b_ip" NODE_C_IP="$node_c_ip" CA_A="$tls_directory/a.crt" CA_B="$tls_directory/b.crt" CA_C="$tls_directory/c.crt" python3 - <<'PY'
import json
import os
import ssl
import time
import urllib.error
import urllib.request

ports = {"a": 18781, "b": 18782, "c": 18783}
tokens = {"a": os.environ["TOKEN_A"], "b": os.environ["TOKEN_B"], "c": os.environ["TOKEN_C"]}
contexts = {node: ssl.create_default_context(cafile=os.environ[f"CA_{node.upper()}"]) for node in ports}

def request(node, path, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    headers = {"Accept": "application/json", "Authorization": "Bearer " + tokens[node]}
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(f"https://127.0.0.1:{ports[node]}{path}", data=data, headers=headers, method="POST" if payload is not None else "GET")
    try:
        with urllib.request.urlopen(request, timeout=15, context=contexts[node]) as response:
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

# The invitation endpoint has to be the exact address the node advertises,
# because the node signs it into the pairing transcript and rejects any other
# value as `pairing_endpoint_mismatch`. The service name this used to send was
# never that address: the node stores a SocketAddr and can only ever advertise
# a literal one.
addresses = {node: os.environ[f"NODE_{node.upper()}_IP"] for node in ports}

def pair(inviter, responder, responder_name):
    invitation = request(inviter, "/api/v1/pair/invitations", {"lifetimeMs": 600000, "endpoints": [f"{addresses[inviter]}:8787"]})
    session = request(responder, "/api/v1/pair/accept", {
        "invitation": invitation, "responderName": responder_name,
        "responderRoles": ["storage_provider", "backup_reader"],
        "inviterRoles": ["backup_writer", "backup_reader"],
    })
    session = request(responder, "/api/v1/pair/confirm/responder", {"session": session, "displayedCode": session["authenticationString"]})
    session = request(inviter, "/api/v1/pair/confirm/inviter", {"session": session, "displayedCode": session["authenticationString"]})
    finalized = request(inviter, "/api/v1/pair/finalize/inviter", {"session": session})
    request(responder, "/api/v1/pair/finalize/responder", {"session": session})
    # `inviterGrant` is the grant the inviter issued, so it describes the
    # responder and carries the roles the responder was granted.
    if "storage_provider" not in finalized["inviterGrant"]["roles"]:
        raise SystemExit(f"{responder_name} was not granted the storage_provider role")
    # The mutually signed binding the inviter durably trusted when it finalized,
    # carried in the pairing session this harness relays between the two nodes.
    # Connecting a provider confirms that exact record - the node compares every
    # field against the one it stored and answers `provider_binding_mismatch` on
    # any difference - so it is taken verbatim rather than reassembled from parts
    # that merely look equivalent.
    #
    # A real client reads this from the `peerTransport` of its own finalize
    # response instead. That field is null on the inviter side today, so it
    # cannot be the source here; the session carries the identical record.
    transport = session["responderTransport"]
    # Cross-check the signed record against what the responder itself reports
    # live. These come from different places - one from the pairing transcript,
    # one from the running node - and a disagreement means the transcript does
    # not describe the node it names.
    identity = request(responder, "/api/v1/transport/identity")
    if transport["peerId"] != identity["deviceId"]:
        raise SystemExit(f"signed transport for {responder_name} names a different device")
    if transport["certificateFingerprint"] != identity["certificateFingerprint"]:
        raise SystemExit(f"signed transport for {responder_name} pins a different certificate")
    # Proves the advertised-address override actually reached the transcript.
    # Without it the node refuses to advertise at all and pairing never gets
    # this far, so this asserts the address is not merely present but correct.
    expected_address = f"{addresses[responder]}:8787"
    if transport["address"] != expected_address:
        raise SystemExit(f"signed transport for {responder_name} advertises {transport['address']}, expected {expected_address}")
    return transport

transport_b = pair("a", "b", "Node B")
transport_c = pair("a", "c", "Node C")
for transport in (transport_b, transport_c):
    request("a", "/api/v1/providers/connect", {"peerTransport": transport})

backup = request("a", "/api/v1/backups", {
    "sourceRoot": "/source", "displayName": "Compose disaster drill", "snapshotId": "compose-snapshot", "jobId": "compose-backup",
    "selectedProviderIds": [transport_b["peerId"], transport_c["peerId"]],
})
if backup["selectedProviders"] != 2 or backup["degradedFailures"] != 0:
    raise SystemExit("explicit two-provider replication did not complete")
availability = request("a", "/api/v1/backups/verify", {"backupId": backup["backupId"], "snapshotId": "compose-snapshot", "verifyProviders": True})
if not availability["intact"] or len(availability["providerAvailability"]) != 2:
    raise SystemExit("provider availability is incomplete")

settings = request("a", "/api/v1/config/export", {})
request("a", "/api/v1/config/import", {"confirmed": True, "settings": settings})
print(json.dumps({"backupId": backup["backupId"], "providerIds": [transport_b["peerId"], transport_c["peerId"]]}))
PY

# Simulate source loss and one local corrupt ciphertext. Repair must use an
# already paired, explicitly selected provider rather than the vanished source.
rm -rf "$source_dir/nested"
$compose exec -T node-a sh -c 'chunk=$(find /data/store/chunks -type f | head -n 1); test -n "$chunk"; printf corrupt > "$chunk"'

# Resolve the immutable backup through the same authenticated listing contract
# used by native and web clients.
backup_id=$(TOKEN_A="$token_a" CA_A="$tls_directory/a.crt" python3 - <<'PY'
import json
import os
import ssl
import urllib.request
import urllib.parse

request = urllib.request.Request(
    "https://127.0.0.1:18781/api/v1/backups",
    headers={"Accept": "application/json", "Authorization": "Bearer " + os.environ["TOKEN_A"]},
)
with urllib.request.urlopen(request, timeout=15, context=ssl.create_default_context(cafile=os.environ["CA_A"])) as response:
    backups = json.load(response)
matches = [backup for backup in backups if backup["latestSnapshotId"] == "compose-snapshot"]
if len(matches) != 1:
    raise SystemExit("backup listing did not return the compose snapshot exactly once")
print(matches[0]["backupId"])
PY
)
TOKEN_A="$token_a" BACKUP_ID="$backup_id" CA_A="$tls_directory/a.crt" python3 - <<'PY'
import json
import os
import ssl
import urllib.request

headers = {"Accept": "application/json", "Authorization": "Bearer " + os.environ["TOKEN_A"], "Content-Type": "application/json"}
context = ssl.create_default_context(cafile=os.environ["CA_A"])
def exchange(path, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    request = urllib.request.Request(
        "https://127.0.0.1:18781" + path,
        data=data,
        headers=headers,
        method="GET" if payload is None else "POST",
    )
    with urllib.request.urlopen(request, timeout=30, context=context) as response:
        body = None if response.status == 204 else json.loads(response.read() or b"null")
        return body, response.headers

def post(path, payload):
    return exchange(path, payload)[0]

request = {"backupId": os.environ["BACKUP_ID"], "snapshotId": "compose-snapshot"}
before = post("/api/v1/backups/verify", request)
if before["intact"]:
    raise SystemExit("corrupt local chunk was not rejected")
after = post("/api/v1/backups/verify", {**request, "repair": True})
if not after["intact"]:
    raise SystemExit("repair from paired provider failed")
plan, plan_headers = exchange("/api/v1/restores/preview", {**request, "targetRoot": "/restore", "conflictPolicy": "fail", "jobId": "compose-restore"})
if plan_headers.get("X-Covalent-Restore-Plan-Id") != plan["planId"] or plan_headers.get("X-Covalent-Restore-Plan-Digest") != plan["planDigest"]:
    raise SystemExit("restore reference headers did not bind the durable signed plan")
entries = []
cursor = None
while True:
    query = urllib.parse.urlencode({"limit": 1000, **({"cursor": cursor} if cursor is not None else {})})
    page, _ = exchange(f"/api/v1/restores/plans/{plan['planId']}?{query}")
    if page["planId"] != plan["planId"] or page["planDigest"] != plan["planDigest"] or page["entryOffset"] != len(entries):
        raise SystemExit("restore plan pagination changed its signed binding or offset")
    entries.extend(page["entries"])
    cursor = page["nextCursor"]
    if cursor is None:
        break
if len(entries) != plan["totalEntries"]:
    raise SystemExit("restore plan pagination did not return every signed entry")
if any(entry["destinationPath"].startswith("/") or ".." in entry["destinationPath"].split("/") for entry in entries):
    raise SystemExit("restore preview escaped its authorized root")
result, result_headers = exchange("/api/v1/restores/execute", {"planId": plan["planId"]})
if result_headers.get("X-Covalent-Restore-Plan-Id") != plan["planId"] or result_headers.get("X-Covalent-Restore-Plan-Digest") != plan["planDigest"]:
    raise SystemExit("restore result headers did not bind the executed plan")
if result["filesRestored"] < 2:
    raise SystemExit("restore after source loss did not restore expected files")
post("/api/v1/jobs/discard", {"jobId": "compose-restore"})
print("compose E2E: pair, explicit replication, source loss, corruption rejection/repair, safe settings import, and root-confined restore: ok")
PY

$compose exec -T node-a test -f /restore/nested/payload.txt
echo "Docker Compose multi-node E2E: ok"

#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
workflow="$repo_root/.github/workflows/container-supply-chain.yml"

python3 - "$workflow" <<'PY'
import base64
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

workflow = Path(sys.argv[1]).read_text().replace("${{ needs.validate.outputs.tag_prefix }}", "v")
version = "0.2.1"
fingerprint = "f" * 64

raw_marker = 'docker buildx imagetools inspect --raw "${staging}" |'
segments = workflow.split(raw_marker)[1:]
if len(segments) != 2:
    raise SystemExit(f"expected two private-index conversion stages, found {len(segments)}")

transform_pattern = re.compile(
    r"jq --arg version .*?\\\n\s+--arg fingerprint .*? '([\s\S]*?)' > private-index-upload\.json"
)
contract_pattern = re.compile(
    r"jq -e \\\n\s+--arg version .*?\\\n\s+--arg fingerprint .*? '([\s\S]*?)' private-index\.json"
)
transforms = []
contracts = []
for segment in segments:
    segment = segment.split('digest=$(jq -r', 1)[0]
    transform = transform_pattern.search(segment)
    contract = contract_pattern.search(segment)
    if not transform or not contract:
        raise SystemExit("could not extract private-index conversion and verification filters")
    transforms.append(transform.group(1))
    contracts.append(contract.group(1))

if transforms[0] != transforms[1] or contracts[0] != contracts[1]:
    raise SystemExit("scan and promotion use different private-index logic")

manifests = [
    {
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "digest": "sha256:" + "a" * 64,
        "size": 111,
        "platform": {"architecture": "amd64", "os": "linux"},
    },
    {
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "digest": "sha256:" + "b" * 64,
        "size": 222,
        "platform": {"architecture": "arm64", "os": "linux"},
    },
]
source = {
    "schemaVersion": 2,
    "mediaType": "application/vnd.docker.distribution.manifest.list.v2+json",
    "manifests": manifests,
}
source_bytes = json.dumps(source, separators=(",", ":")).encode()


def jq(program, payload, check=True, exit_status=False):
    command = ["jq"]
    if exit_status:
        command.append("-e")
    command += ["--arg", "version", version, "--arg", "fingerprint", fingerprint, program]
    return subprocess.run(
        command,
        input=payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=check,
    )


if jq(contracts[0], source_bytes, check=False, exit_status=True).returncode == 0:
    raise SystemExit("unmodified Docker manifest list unexpectedly passed the OCI index contract")

outputs = [jq(program, source_bytes).stdout for program in transforms]
if outputs[0] != outputs[1]:
    raise SystemExit("scan and promotion produced different private-index bytes")

converted = json.loads(outputs[0])
if converted["mediaType"] != "application/vnd.oci.image.index.v1+json":
    raise SystemExit("conversion did not produce an OCI index")
if converted.get("annotations") != {
    "org.opencontainers.image.version": version,
    "io.covalent.source.fingerprint": fingerprint,
    "io.covalent.release.tag-prefix": "v",
}:
    raise SystemExit("conversion did not preserve both release annotations")
if converted["manifests"] != manifests:
    raise SystemExit("conversion changed child manifest descriptors")
jq(contracts[0], outputs[0], exit_status=True)

# Cosign 2.5 emits one verified DSSE envelope as an object. Older releases
# emitted an array. The workflow must accept both without changing the signed
# in-toto statement or weakening exact SPDX equality.
verification_shape = re.search(
    r"jq -e '(if type == \"array\" then length > 0 else type == \"object\" end)'",
    workflow,
)
payload_shape = re.search(
    r"jq -r '(if type == \"array\" then \.\[\]\.payload else \.payload end)'",
    workflow,
)
if not verification_shape or not payload_shape:
    raise SystemExit("missing Cosign object/array output normalization")

statement = {
    "_type": "https://in-toto.io/Statement/v0.1",
    "subject": [{"name": "fixture", "digest": {"sha256": "c" * 64}}],
    "predicateType": "https://spdx.dev/Document",
    "predicate": {"spdxVersion": "SPDX-2.3", "SPDXID": "SPDXRef-DOCUMENT"},
}
payload = base64.b64encode(json.dumps(statement, separators=(",", ":")).encode()).decode()
envelope = {"payloadType": "application/vnd.in-toto+json", "payload": payload, "signatures": []}
for verification in (envelope, [envelope]):
    shape = subprocess.run(
        ["jq", "-e", verification_shape.group(1)],
        input=json.dumps(verification).encode(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if shape.returncode:
        raise SystemExit("valid Cosign verification shape was rejected")
    extracted = subprocess.run(
        ["jq", "-r", payload_shape.group(1)],
        input=json.dumps(verification).encode(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    ).stdout.decode().splitlines()
    if extracted != [payload]:
        raise SystemExit("Cosign payload normalization changed the signed payload")

decoded = json.loads(base64.b64decode(payload))
if decoded["predicate"] != statement["predicate"]:
    raise SystemExit("fixture predicate changed during DSSE extraction")

# Parse every shell run step after replacing Actions expressions with a benign
# word. This catches quoting or heredoc damage in the workflow itself.
lines = workflow.splitlines()
run_steps = []
i = 0
while i < len(lines):
    match = re.match(r"^(\s*)run:\s*([|>])-?\s*$", lines[i])
    if not match:
        i += 1
        continue
    indent = len(match.group(1))
    style = match.group(2)
    body = []
    i += 1
    while i < len(lines):
        line = lines[i]
        if line.strip() and len(line) - len(line.lstrip()) <= indent:
            break
        body.append(line[indent + 2 :] if line.strip() else "")
        i += 1
    script = "\n".join(body) if style == "|" else " ".join(part.strip() for part in body)
    run_steps.append(re.sub(r"\$\{\{.*?\}\}", "FIXTURE", script))

if not run_steps:
    raise SystemExit("no workflow run steps found")
for number, script in enumerate(run_steps, 1):
    with tempfile.NamedTemporaryFile("w", suffix=".bash") as fixture:
        fixture.write(script)
        fixture.flush()
        parsed = subprocess.run(["bash", "-n", fixture.name], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if parsed.returncode:
        raise SystemExit(f"workflow run step {number} is not valid Bash: {parsed.stderr.decode().strip()}")

print(f"container OCI index fixture: ok ({len(run_steps)} workflow run steps parsed)")
PY

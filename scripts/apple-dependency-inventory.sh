#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output_directory=${1:-$repo_root/apps/apple/.build/reports/covalent}
resolved="$repo_root/apps/apple/Package.resolved"
notices="$repo_root/THIRD_PARTY_NOTICES.md"

test -f "$resolved"
test -f "$notices"
mkdir -p "$output_directory"

source_revision=${GITHUB_SHA:-$(git -C "$repo_root" rev-parse HEAD)}
SOURCE_REVISION="$source_revision" \
RESOLVED_PATH="$resolved" \
OUTPUT_DIRECTORY="$output_directory" \
python3 <<'PY'
import datetime
import hashlib
import json
import os
import uuid
from pathlib import Path
from urllib.parse import quote

resolved_path = Path(os.environ["RESOLVED_PATH"])
output_directory = Path(os.environ["OUTPUT_DIRECTORY"])
source_revision = os.environ["SOURCE_REVISION"]
resolved_bytes = resolved_path.read_bytes()
resolved = json.loads(resolved_bytes)

approved = {
    "zipfoundation": {
        "display": "ZIPFoundation",
        "license": "MIT",
    },
}

components = []
packages = []
relationships = []
for pin in sorted(resolved.get("pins", []), key=lambda value: value["identity"]):
    identity = pin["identity"].lower()
    if identity not in approved:
        raise SystemExit(f"Apple dependency has no reviewed license mapping: {identity}")
    state = pin["state"]
    version = state.get("version") or state.get("revision")
    location = pin["location"]
    metadata = approved[identity]
    spdx_id = "SPDXRef-Package-" + "".join(character if character.isalnum() else "-" for character in identity)
    purl = f"pkg:swift/{quote(metadata['display'])}@{quote(version)}?vcs_url={quote(location, safe='')}"
    components.append(
        {
            "type": "library",
            "bom-ref": purl,
            "name": metadata["display"],
            "version": version,
            "licenses": [{"license": {"id": metadata["license"]}}],
            "purl": purl,
            "externalReferences": [{"type": "vcs", "url": location}],
            "properties": [
                {"name": "covalent:swift-revision", "value": state["revision"]},
            ],
        }
    )
    packages.append(
        {
            "SPDXID": spdx_id,
            "name": metadata["display"],
            "versionInfo": version,
            "downloadLocation": location,
            "filesAnalyzed": False,
            "licenseConcluded": metadata["license"],
            "licenseDeclared": metadata["license"],
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": purl,
                }
            ],
        }
    )
    relationships.append(
        {
            "spdxElementId": "SPDXRef-Package-CovalentApple",
            "relationshipType": "DEPENDS_ON",
            "relatedSpdxElement": spdx_id,
        }
    )

serial_digest = hashlib.sha256(resolved_bytes + source_revision.encode()).hexdigest()
timestamp = datetime.datetime.fromtimestamp(
    int(os.environ.get("SOURCE_DATE_EPOCH", "0")), datetime.timezone.utc
).strftime("%Y-%m-%dT%H:%M:%SZ")

cyclonedx = {
    "bomFormat": "CycloneDX",
    "specVersion": "1.5",
    "serialNumber": f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, serial_digest)}",
    "version": 1,
    "metadata": {
        "timestamp": timestamp,
        "component": {
            "type": "application",
            "name": "Covalent Apple clients",
            "version": "0.1.0",
            "properties": [{"name": "covalent:source-revision", "value": source_revision}],
        },
    },
    "components": components,
}

spdx = {
    "spdxVersion": "SPDX-2.3",
    "dataLicense": "CC0-1.0",
    "SPDXID": "SPDXRef-DOCUMENT",
    "name": "Covalent-Apple-dependencies",
    "documentNamespace": f"https://github.com/thekozugroup/Covalent/spdx/apple/{serial_digest}",
    "creationInfo": {
        "created": timestamp,
        "creators": ["Tool: scripts/apple-dependency-inventory.sh"],
    },
    "documentDescribes": ["SPDXRef-Package-CovalentApple"],
    "packages": [
        {
            "SPDXID": "SPDXRef-Package-CovalentApple",
            "name": "Covalent Apple clients",
            "versionInfo": "0.1.0",
            "downloadLocation": "https://github.com/thekozugroup/Covalent",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "OTHER",
                    "referenceType": "cpe23Type",
                    "referenceLocator": f"source-revision:{source_revision}",
                }
            ],
        },
        *packages,
    ],
    "relationships": relationships,
}

for filename, payload in (
    ("apple-sbom.cdx.json", cyclonedx),
    ("apple-sbom.spdx.json", spdx),
):
    (output_directory / filename).write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
PY

cp "$resolved" "$output_directory/Package.resolved"
cp "$notices" "$output_directory/THIRD_PARTY_NOTICES.md"
(
  cd "$output_directory"
  shasum -a 256 \
    Package.resolved \
    THIRD_PARTY_NOTICES.md \
    apple-sbom.cdx.json \
    apple-sbom.spdx.json > SHA256SUMS
)

printf '%s\n' "Apple dependency inventory written to $output_directory"

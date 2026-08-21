#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output_directory=${1:-$repo_root/apps/apple/.build/reports/covalent}
resolved="$repo_root/apps/apple/Package.resolved"
notices="$repo_root/THIRD_PARTY_NOTICES.md"

test -f "$resolved"
test -f "$notices"
mkdir -p "$output_directory"

manifest="$repo_root/apps/apple/Package.swift"
test -f "$manifest"

source_revision=${GITHUB_SHA:-$(git -C "$repo_root" rev-parse HEAD)}
SOURCE_REVISION="$source_revision" \
RESOLVED_PATH="$resolved" \
MANIFEST_PATH="$manifest" \
OUTPUT_DIRECTORY="$output_directory" \
python3 <<'PY'
import datetime
import hashlib
import json
import os
import re
import uuid
from pathlib import Path
from urllib.parse import quote

resolved_path = Path(os.environ["RESOLVED_PATH"])
manifest_path = Path(os.environ["MANIFEST_PATH"])
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

# `resolved.get("pins", [])` used to be the whole of the pin discovery. SwiftPM
# writes v1/v2 files with the pins nested under "object", so on those schemas the
# loop below ran zero times: the unreviewed-license gate never fired and an SBOM
# with `components: []` was written, published as a release asset, and accepted by
# CI's `if-no-files-found: error` because the file did exist - it was just empty.
# Locate the pins explicitly and refuse a schema this script does not understand.
if isinstance(resolved.get("pins"), list):
    pins = resolved["pins"]
elif isinstance(resolved.get("object"), dict) and isinstance(
    resolved["object"].get("pins"), list
):
    pins = resolved["object"]["pins"]
else:
    raise SystemExit(
        "Unrecognised Package.resolved schema "
        f"(version={resolved.get('version')!r}); refusing to emit an empty SBOM. "
        f"Top-level keys: {sorted(resolved)}"
    )


def pin_identity(pin):
    identity = pin.get("identity") or pin.get("package")
    if not identity:
        raise SystemExit(f"Package.resolved pin has no identity: {pin!r}")
    return identity.lower()


# Cross-check against the manifest so a resolution that silently lost a declared
# dependency cannot produce a short, clean-looking inventory. Every package the
# manifest declares must appear among the pins.
manifest_source = manifest_path.read_text(encoding="utf-8")
declared_urls = set(re.findall(r'\.package\(\s*url:\s*"([^"]+)"', manifest_source))
pinned_urls = {
    (pin.get("location") or pin.get("repositoryURL") or "").rstrip("/") for pin in pins
}
missing_urls = sorted(
    url for url in declared_urls if url.rstrip("/") not in pinned_urls
)
if missing_urls:
    raise SystemExit(
        "Package.resolved does not pin these dependencies declared in "
        f"{manifest_path.name}: {missing_urls}"
    )
if declared_urls and not pins:
    raise SystemExit(
        f"{manifest_path.name} declares {len(declared_urls)} dependencies but "
        "Package.resolved pinned none; refusing to emit an empty SBOM."
    )

components = []
packages = []
relationships = []
for pin in sorted(pins, key=pin_identity):
    identity = pin_identity(pin)
    if identity not in approved:
        raise SystemExit(f"Apple dependency has no reviewed license mapping: {identity}")
    state = pin["state"]
    version = state.get("version") or state.get("revision")
    location = pin.get("location") or pin.get("repositoryURL")
    if not location:
        raise SystemExit(f"Package.resolved pin has no location: {identity}")
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

if len(components) != len(pins):
    raise SystemExit(
        f"Emitted {len(components)} SBOM components for {len(pins)} resolved pins."
    )

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

# Say what was actually inventoried. A gate that prints only "done" cannot be
# told apart from a gate that did nothing, which is exactly how an empty SBOM
# shipped as a release asset.
component_count=$(python3 -c '
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    print(len(json.load(handle)["components"]))
' "$output_directory/apple-sbom.cdx.json")
printf '%s\n' "Apple dependency inventory written to $output_directory ($component_count reviewed components)"

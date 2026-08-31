#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
device_gate="$repo_root/scripts/android-api37-device-test.sh"
dockerfile="$repo_root/packaging/docker/Dockerfile"
ci_workflow="$repo_root/.github/workflows/ci.yml"

"$repo_root/scripts/test-docker-source-fingerprint.sh"

# The OCI label and both device-gate paths are coupled: a full build must
# create and verify a content label, while a prebuilt must require that exact
# current digest instead of accepting a matching HEAD alone.
grep -Fq 'ARG COVALENT_SOURCE_FINGERPRINT=unknown' "$dockerfile"
grep -Fq 'io.covalent.source.fingerprint="$COVALENT_SOURCE_FINGERPRINT"' "$dockerfile"
grep -Fq 'capture_docker_source_fingerprint' "$device_gate"
grep -Fq -- '--build-arg "COVALENT_SOURCE_FINGERPRINT=$docker_source_fingerprint"' "$device_gate"
grep -Fq '[ "$image_source_fingerprint" != "$expected_source_fingerprint" ]' "$device_gate"
grep -Fq 'Docker source changed while $tls_image was being built' "$device_gate"
grep -Fq 'COVALENT_SOURCE_FINGERPRINT=$(./scripts/docker-source-fingerprint.sh .' "$ci_workflow"

# A Bloop device is rejected by serial unless it has impersonated the pinned
# port; in that case its AVD name is rejected. Keep both exact branches pinned
# so future refactors cannot silently broaden accepted device identity.
grep -Fq 'avd_name=Covalent_API_37' "$device_gate"
grep -Fq 'required_serial=emulator-5570' "$device_gate"
grep -Fq '[ "$serial" != "$required_serial" ]' "$device_gate"
grep -Fq 'Refusing Android serial $serial; this gate is pinned to $required_serial.' "$device_gate"
grep -Fq '[ "$running_avd" != "$avd_name" ]' "$device_gate"
grep -Fq '$serial is $running_avd, not required AVD $avd_name.' "$device_gate"
grep -Fq '[ "$api_level" != "37" ]' "$device_gate"

echo "Android prebuilt freshness and dedicated-device contracts: ok"

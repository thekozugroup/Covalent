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

# GNU stat -f can emit filesystem details before failing, contaminating a
# command substitution before its BSD fallback appends the actual file mode.
# Keep the device fixture's host-mode probe ordered and its failure explicit.
gnu_stat_line=$(grep -n "stat -c '%a'" "$device_gate" | head -n 1 | cut -d: -f1)
bsd_stat_line=$(grep -n "stat -f '%Lp'" "$device_gate" | head -n 1 | cut -d: -f1)
if [[ -z "$gnu_stat_line" || -z "$bsd_stat_line" || "$gnu_stat_line" -ge "$bsd_stat_line" ]]; then
  echo "Android device gate must try GNU stat -c before BSD stat -f" >&2
  exit 1
fi
bare_mode_test_pattern='^[[:space:]]*test[[:space:]]+"\$mode"[[:space:]]*=[[:space:]]*600[[:space:]]*$'
if grep -Eq "$bare_mode_test_pattern" "$device_gate"; then
  echo "Android device gate must diagnose an unexpected fixture-secret mode" >&2
  exit 1
fi
mode_diagnostic='provisioned Android fixture KEK mode is $mode; expected 600'
grep -Fq "$mode_diagnostic" "$device_gate"

echo "Android prebuilt freshness and dedicated-device contracts: ok"

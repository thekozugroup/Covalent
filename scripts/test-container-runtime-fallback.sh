#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
platform_tool="$repo_root/scripts/container-runtime-platform.sh"
runtime_script="$repo_root/scripts/check-container-runtime.sh"

# Docker Desktop reports Apple Silicon as aarch64 while OCI images use arm64.
# Both spellings must remain native so a healthy arm64 release candidate never
# takes the emulation-only fallback.
test "$("$platform_tool" aarch64 arm64)" = native
test "$("$platform_tool" arm64 arm64)" = native
test "$("$platform_tool" x86_64 amd64)" = native
test "$("$platform_tool" aarch64 amd64)" = cross-arch
if "$platform_tool" aarch64 s390x >/dev/null 2>&1; then
  echo "unsupported OCI architecture was accepted" >&2
  exit 1
fi

# The fallback is deliberately narrower than a generic retry: it requires the
# one documented host LibreSSL/QEMU handshake signature and an open IPv4
# publish mapping, then keeps localhost as the TLS hostname in the isolated
# same-architecture client probe.
grep -Fq 'arch_mode=$("$platform_tool" "$host_arch" "$image_arch")' "$runtime_script"
grep -Fq '[ "$arch_mode" != cross-arch ]' "$runtime_script"
grep -Fq 'curl: \(35\) LibreSSL/[0-9.]+: .*bad decrypt' "$runtime_script"
grep -Fq 'nc -z -w 3 127.0.0.1 "$https_port"' "$runtime_script"
grep -Fq 'cross-arch emulation fallback' "$runtime_script"
grep -Fq -- '--connect-to localhost:8443:runtime-node:8443' "$runtime_script"
grep -Fq -- '--cacert /client/root.crt' "$runtime_script"
grep -Fq 'https://localhost:8443/api/v1/config/export' "$runtime_script"

echo "Container runtime cross-architecture fallback contract: ok"

#!/bin/sh
set -eu

: "${COVALENT_REAL_CLANG:?Missing exact NDK Clang path}"
: "${COVALENT_LINK_CLASSIFIER:?Missing link classifier path}"
: "${COVALENT_LINK_PRIVATE_ROOT:?Missing private build root}"
: "${COVALENT_LINK_MAP:?Missing final link map path}"
: "${COVALENT_DRIVER_TRACE:?Missing final driver trace path}"
: "${COVALENT_LINK_MARKER:?Missing final-link marker path}"

kind=$(python3 "$COVALENT_LINK_CLASSIFIER" \
  --classify-go-link \
  --private-root "$COVALENT_LINK_PRIVATE_ROOT" \
  -- "$@")
case "$kind" in
  probe)
    exec "$COVALENT_REAL_CLANG" "$@"
    ;;
  final)
    mkdir "$COVALENT_LINK_MARKER" 2>/dev/null || {
      echo "Syncthing final external linker was invoked more than once" >&2
      exit 1
    }
    "$COVALENT_REAL_CLANG" -### "$@" "-Wl,-Map,$COVALENT_LINK_MAP" \
      >/dev/null 2> "$COVALENT_DRIVER_TRACE"
    trace_size=$(wc -c < "$COVALENT_DRIVER_TRACE" | tr -d '[:space:]')
    test "$trace_size" -gt 0 && test "$trace_size" -le 4194304 || {
      echo "Syncthing Clang driver trace is empty or exceeds 4 MiB" >&2
      exit 1
    }
    exec "$COVALENT_REAL_CLANG" "$@" "-Wl,-Map,$COVALENT_LINK_MAP"
    ;;
  *)
    echo "External-link classifier returned an invalid result" >&2
    exit 1
    ;;
esac

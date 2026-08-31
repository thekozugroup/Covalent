#!/bin/sh
set -eu

# Containers set UID/GID through Docker's --user option.  The image never starts
# as root, so it cannot silently change ownership of host-mounted paths.
# Compose deliberately fixes this identity to 65532:65532 because file-backed
# secrets retain host ownership. Refuse common PUID/PGID overrides instead of
# silently accepting an identity that cannot read an owner-only KEK.
if [ "${PUID+x}" = x ] || [ "${PGID+x}" = x ]; then
  echo "PUID/PGID overrides are unsupported: Covalent Compose runs as fixed UID/GID 65532:65532 so its owner-only KEK remains readable" >&2
  exit 64
fi

case "${UMASK:-027}" in
  0[0-7][0-7]) umask "$UMASK" ;;
  *) echo "UMASK must be a three-digit octal value such as 027" >&2; exit 64 ;;
esac

# Provisioning a KEK is an explicit, one-time operator action. It does not need
# the daemon mounts and must never accidentally start Caddy or generate state.
if [ "${1:-serve}" != "serve" ]; then
  exec covalent-node "$@"
fi

for directory in "${COVALENT_CONFIG_DIR:-/config}" "${COVALENT_DATA_DIR:-/data}"; do
  if [ ! -d "$directory" ] || [ ! -w "$directory" ]; then
    echo "Covalent requires a writable durable mount at $directory" >&2
    exit 73
  fi
done

# The KEK is deliberately outside both appdata mounts. Copying /config and
# /data without this separately protected secret keeps the copied state locked.
: "${COVALENT_KEY_ENCRYPTION_KEY_FILE:=/run/secrets/covalent-kek}"
: "${COVALENT_KEY_ENCRYPTION_KEY_VERSION:=1}"
export COVALENT_KEY_ENCRYPTION_KEY_FILE COVALENT_KEY_ENCRYPTION_KEY_VERSION
if [ ! -f "$COVALENT_KEY_ENCRYPTION_KEY_FILE" ] || [ -L "$COVALENT_KEY_ENCRYPTION_KEY_FILE" ]; then
  echo "Covalent is locked: required KEK file is missing at $COVALENT_KEY_ENCRYPTION_KEY_FILE" >&2
  echo "Provision it once on a trusted operator machine, mount it read-only, and keep the same non-zero version for this state directory. Covalent never generates a replacement KEK." >&2
  exit 78
fi

mkdir -p "${XDG_DATA_HOME:-/config/caddy/data}" "${XDG_CONFIG_HOME:-/config/caddy/config}"

# The node hands this certificate to a client that proves it holds the first-run
# setup code, so nobody has to copy a .crt out of a container by hand.  Caddy
# writes it on first start; the node reads it lazily, at claim time, by which
# point the client has necessarily already completed a TLS handshake through
# Caddy and the file therefore exists.
COVALENT_TLS_CA_FILE="${COVALENT_TLS_CA_FILE:-${XDG_DATA_HOME:-/config/caddy/data}/caddy/pki/authorities/local/root.crt}"
export COVALENT_TLS_CA_FILE

# Bridge networking is Unraid's default, so the address the container can see is
# its own bridge address and peers must dial the host instead.  The container
# cannot observe the host's address, but the operator already told us the exact
# name clients use, so resolve that rather than making them state it twice.
# Purely a convenience: if this resolves to nothing the node auto-detects, and
# if that also fails it refuses to advertise and says what to set.
if [ -z "${COVALENT_ADVERTISED_PEER_ADDRESS:-}" ]; then
  # Compose must expose the optional override, but an empty environment value
  # is not a valid SocketAddr for clap. Remove it unless resolution below
  # produces the concrete numeric IP:port the node contract accepts.
  unset COVALENT_ADVERTISED_PEER_ADDRESS
fi
if [ -n "${COVALENT_HTTPS_HOST:-}" ] && [ -z "${COVALENT_ADVERTISED_PEER_ADDRESS:-}" ]; then
  resolved=$(getent ahostsv4 "$COVALENT_HTTPS_HOST" 2>/dev/null | awk 'NR==1 {print $1}')
  case "$resolved" in
    ''|127.*) : ;;
    *)
      peer_port=${COVALENT_PEER_LISTEN##*:}
      case "$peer_port" in
        ''|*[!0-9]*) peer_port=8787 ;;
      esac
      COVALENT_ADVERTISED_PEER_ADDRESS="$resolved:$peer_port"
      export COVALENT_ADVERTISED_PEER_ADDRESS
      echo "Covalent will tell other devices to dial $COVALENT_ADVERTISED_PEER_ADDRESS (resolved from $COVALENT_HTTPS_HOST)"
      ;;
  esac
fi

# The Rust CLI contract is SocketAddr, not a DNS name. Reject hostname-shaped
# overrides here with an operator-facing message rather than letting clap emit
# a generic parse error. DNS belongs in COVALENT_HTTPS_HOST; leaving this value
# empty lets the resolution block above produce a concrete address.
if [ -n "${COVALENT_ADVERTISED_PEER_ADDRESS:-}" ]; then
  advertised_ip=''
  advertised_port=${COVALENT_ADVERTISED_PEER_ADDRESS##*:}
  if [ "${#COVALENT_ADVERTISED_PEER_ADDRESS}" -gt 80 ]; then
    advertised_port=invalid
  fi
  case "$advertised_port" in
    ''|*[!0-9]*|??????*) advertised_port=invalid ;;
  esac
  if [ "$advertised_port" = invalid ] || [ "$advertised_port" -lt 1 ] \
    || [ "$advertised_port" -gt 65535 ]; then
    echo "COVALENT_ADVERTISED_PEER_ADDRESS must be a numeric IP:port, not a hostname" >&2
    exit 64
  fi

  case "$COVALENT_ADVERTISED_PEER_ADDRESS" in
    \[*\]:*)
      advertised_ip=${COVALENT_ADVERTISED_PEER_ADDRESS#\[}
      advertised_ip=${advertised_ip%%\]*}
      if [ "$COVALENT_ADVERTISED_PEER_ADDRESS" != "[$advertised_ip]:$advertised_port" ]; then
        advertised_ip=''
      fi
      # Validate a hexadecimal IPv6 literal without DNS or network access.
      # IPv4-embedded forms are intentionally not accepted here; operators can
      # always use the host's stable Tailnet IPv4 address instead.
      if [ -n "$advertised_ip" ] && ! awk -v address="$advertised_ip" '
        function count_groups(text, groups, count, position) {
          if (text == "") return 0
          count = split(text, groups, ":")
          for (position = 1; position <= count; position++) {
            if (groups[position] == "" || length(groups[position]) > 4 \
              || groups[position] !~ /^[0-9A-Fa-f]+$/) return -1
          }
          return count
        }
        BEGIN {
          if (address == "" || address ~ /:::/) exit 1
          compression = index(address, "::")
          if (compression > 0) {
            left = substr(address, 1, compression - 1)
            right = substr(address, compression + 2)
            if (index(right, "::") > 0) exit 1
            left_count = count_groups(left)
            right_count = count_groups(right)
            if (left_count < 0 || right_count < 0 || left_count + right_count >= 8) exit 1
          } else if (count_groups(address) != 8) {
            exit 1
          }
        }
      '; then
        advertised_ip=''
      fi
      ;;
    *:*)
      advertised_ip=${COVALENT_ADVERTISED_PEER_ADDRESS%:*}
      if ! awk -v address="$advertised_ip" 'BEGIN {
        count = split(address, octet, ".")
        if (count != 4) exit 1
        for (position = 1; position <= 4; position++) {
          if (octet[position] !~ /^[0-9]+$/ || octet[position] < 0 || octet[position] > 255) exit 1
        }
      }'; then
        advertised_ip=''
      fi
      ;;
  esac
  if [ -z "$advertised_ip" ]; then
    echo "COVALENT_ADVERTISED_PEER_ADDRESS must be a numeric IP:port, not a hostname" >&2
    exit 64
  fi
fi

covalent-node "$@" &
node_pid=$!
caddy run --config /etc/caddy/Caddyfile --adapter caddyfile &
proxy_pid=$!

shutdown() {
  trap - INT TERM
  kill -TERM "$proxy_pid" "$node_pid" >/dev/null 2>&1 || true
  wait "$proxy_pid" >/dev/null 2>&1 || true
  wait "$node_pid" >/dev/null 2>&1 || true
  exit 0
}
trap shutdown INT TERM

while kill -0 "$node_pid" >/dev/null 2>&1 && kill -0 "$proxy_pid" >/dev/null 2>&1; do
  sleep 1
done

echo "Covalent node or TLS proxy exited unexpectedly" >&2
kill -TERM "$proxy_pid" "$node_pid" >/dev/null 2>&1 || true
wait "$proxy_pid" >/dev/null 2>&1 || true
wait "$node_pid" >/dev/null 2>&1 || true
exit 1

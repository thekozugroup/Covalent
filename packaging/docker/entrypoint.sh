#!/bin/sh
set -eu

# Containers set UID/GID through Docker's --user option.  The image never starts
# as root, so it cannot silently change ownership of host-mounted paths.
case "${UMASK:-027}" in
  0[0-7][0-7]) umask "$UMASK" ;;
  *) echo "UMASK must be a three-digit octal value such as 027" >&2; exit 64 ;;
esac

for directory in "${COVALENT_CONFIG_DIR:-/config}" "${COVALENT_DATA_DIR:-/data}"; do
  if [ ! -d "$directory" ] || [ ! -w "$directory" ]; then
    echo "Covalent requires a writable durable mount at $directory" >&2
    exit 73
  fi
done

# /config is intentionally durable for operator-managed files such as exported
# safe settings.  The daemon's encrypted state and keys remain together in /data.
if [ "${1:-serve}" != "serve" ]; then
  exec covalent-node "$@"
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
if [ -z "${COVALENT_ADVERTISED_PEER_ADDRESS:-}" ] && [ -n "${COVALENT_HTTPS_HOST:-}" ]; then
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

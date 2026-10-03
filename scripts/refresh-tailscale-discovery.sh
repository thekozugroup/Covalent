#!/bin/sh
# Run on the Docker host once a minute. The container receives only this bounded,
# expiring list of online peer names and addresses, never a Tailscale socket.
set -eu

if [ "$#" -ne 1 ] || [ ! -d "$1" ] || [ ! -w "$1" ]; then
  echo "usage: $0 HOST_CONFIG_DIRECTORY (existing writable Covalent /config mount)" >&2
  exit 64
fi
for tool in tailscale jq timeout; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "Covalent discovery refresh requires $tool on the host" >&2
    exit 69
  fi
done

config_dir=$(CDPATH= cd -- "$1" && pwd)
# A private directory on the same filesystem keeps raw host status private and
# makes the final rename atomic. Failed refreshes leave the previous timestamp.
umask 077
work=$(mktemp -d "$config_dir/.tailscale-discovery.XXXXXX")
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM

# Limit the CLI's output file as well as the JSON reader. ulimit -f is measured
# in 512-byte blocks by the POSIX shells supported on these hosts.
(ulimit -f 8192; timeout 5 tailscale status --json > "$work/status.json")
if [ "$(wc -c < "$work/status.json")" -gt 4194304 ]; then
  echo "Tailscale status exceeds the 4 MiB discovery limit" >&2
  exit 65
fi
generated_at=$(($(date +%s) * 1000))
jq -ce --argjson generatedAtUnixMs "$generated_at" '
  def tailnet_ip:
    if type != "string" then false
    elif test("^100\\.[0-9]+\\.[0-9]+\\.[0-9]+$") then
      split(".") | map(tonumber) |
      .[1] >= 64 and .[1] <= 127 and .[2] <= 255 and .[3] <= 255
    else test("^fd7a:115c:a1e0:"; "i") and length <= 39 end;
  if .BackendState != "Running" or (.Peer | type) != "object" then
    error("Tailscale is not running or returned no peer directory")
  else
    {schemaVersion: 1, generatedAtUnixMs: $generatedAtUnixMs, status: {Peer:
      [.Peer[] | select(.Online == true) |
        {DNSName: (.DNSName // .HostName),
         TailscaleIPs: [(.TailscaleIPs // [])[] | select(tailnet_ip)]} |
        select((.DNSName | type) == "string") |
        select((.DNSName | length) > 0 and (.DNSName | length) <= 253) |
        select((.DNSName | test("[\\x00-\\x1f\\x7f]")) | not) |
        select((.TailscaleIPs | length) > 0) |
        .TailscaleIPs |= .[:2]
      ][:256] | to_entries | map({key: (.key | tostring), value: .value}) | from_entries
    }}
  end
' "$work/status.json" > "$work/snapshot.json"
# Peer hints are not credentials. The existing config directory controls access;
# readability must survive both Compose UID 65532 and Unraid UID 99.
chmod 644 "$work/snapshot.json"
mv -f "$work/snapshot.json" "$config_dir/tailscale-discovery.json"

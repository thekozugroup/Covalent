# Tailscale discovery for Docker and Unraid

Covalent can list online devices on the host's Tailnet without receiving the
host's Tailscale socket or running another Tailscale daemon. The host refreshes a
small file in the existing **Configuration** mount once a minute. The node reads
that file when the device list is opened. Hints expire after three minutes; a
failed refresh never renews an old file's timestamp.

These are online Tailnet devices, not verified Covalent servers. Pairing still
requires a running Covalent peer endpoint, matching signed addresses and
certificates, and confirmation of the same code on both devices.

## Before enabling discovery

Use the host's existing `tailscale` CLI, `jq`, and `timeout`. The refresh runs on the host,
outside Docker. It copies only online peer DNS names and Tailnet addresses;
account records, public keys, routes, and the rest of host status are omitted.

Use the standard **8787/UDP** peer port on each host for automatic candidates.
The directory does not contain per-application port mappings. Custom host ports
still require entering the other server's explicit address and port. Set each
node's advertised address to its own Tailnet IPv4 and published peer port, and
allow peer traffic in both directions. HTTPS access alone does not enable peer
pairing. See [Atlas networking](atlas-tailscale.md#4-choose-discovery-or-routing-deliberately).

Use the script from the same verified Covalent checkout as the release. Replace
the paths below with the host directory actually mapped to `/config`. Do not use
the encrypted data directory, a share, or a directory containing the KEK.

## Linux Docker host

Install or update the script, then run it once:

```sh
sudo install -m 755 scripts/refresh-tailscale-discovery.sh /usr/local/sbin/covalent-tailnet-refresh
sudo /usr/local/sbin/covalent-tailnet-refresh /srv/covalent/config
```

Create `/etc/cron.d/covalent-tailnet-discovery` with the following contents,
owned by root and mode `0644`. This persists across reboots on hosts using cron.
Adjust the PATH if the host installs `tailscale` elsewhere.

```cron
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
* * * * * root /usr/local/sbin/covalent-tailnet-refresh /srv/covalent/config
```

The script publishes `tailscale-discovery.json` atomically with mode `0644`, so
the existing container UID can read it through its private configuration
directory. The temporary raw status is mode `0600` in a mode `0700` directory
and is removed on success or failure. No new Docker mount or privileges are
needed. The standard entrypoint supplies `COVALENT_CONFIG_DIR=/config`.

## Unraid

`/etc` and `/usr/local` are recreated at boot on Unraid. Keep the helper on the
persistent flash drive and use the existing **User Scripts** plugin to schedule
it, instead of editing the live crontab. The helper contains no secrets:

```sh
install -d -m 755 /boot/config/covalent
install -m 644 scripts/refresh-tailscale-discovery.sh /boot/config/covalent/refresh-tailscale-discovery.sh
sh /boot/config/covalent/refresh-tailscale-discovery.sh /mnt/user/appdata/covalent/config
```

Create a User Scripts entry named **Covalent Tailnet discovery** with:

```sh
#!/bin/sh
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
exec sh /boot/config/covalent/refresh-tailscale-discovery.sh /mnt/user/appdata/covalent/config
```

Select **Custom** schedule `* * * * *` and apply it. The plugin retains the job
across reboots. If the Tailscale plugin exposes its CLI in another directory,
add that directory to this script's PATH. The helper reports a missing CLI or
`jq` without changing the last successful snapshot. No package is installed in
the Covalent container.

## Verify, update, or disable

Open **Add device** and refresh the device list. The WebUI distinguishes an
available Tailnet directory with no online peers from unavailable, expired, or
failed discovery. The authenticated API exposes the same information at
`GET /api/v1/discovery?details=true`; without the query it retains the existing
candidate-array response for native clients.

When updating Covalent, replace the installed helper using the same `install`
command above. Keep the schedule and config path. Normal container updates
retain the mount and do not require recreating the schedule.

To disable this feed, remove the cron entry or disable the Unraid User Script,
then delete only `tailscale-discovery.json` from the host configuration
directory. Manual address pairing continues to work. Native installations can
still use their existing local Tailscale CLI or LocalAPI discovery.

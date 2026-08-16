# Unraid operation

Unraid is Tier 1. The template maps:

- `/config`: durable application configuration.
- `/data`: encrypted chunk and metadata storage.
- `/source`: one or more explicitly selected `/mnt/user/<share>` paths, read-only by default.
- `/boot-source`: optional `/boot` mapping for backup, read-only.
- `/restore`: optional, explicit writable destination for restores.

Do not map `/mnt/user` or `/boot` writable as a convenience. Preview and choose a conflict policy before restore. Host networking improves mDNS but expands the network boundary; bridge networking plus explicit ports or Tailnet routing remains supported. Tailscale is optional and never replaces Covalent pairing.

The template runs as Unraid's unprivileged `99:100` identity; Docker defaults to `65532:65532`. Change the explicit runtime user only when selected mount ownership requires it. Covalent never needs privileged mode or added Linux capabilities.

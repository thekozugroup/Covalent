# Isolated Mac–Atmos recovery drill

Status: passed as development evidence; repeat on the final release revision.
Atmos is Ubuntu arm64. The owner later confirmed Atlas is offline and accepted
Docker validation as its completion path; no physical Atlas install is claimed.

The drill ran through `scripts/test-remote-drill.sh` with generated temporary
content only. Its source base was `69a6106cbfbd6960c7fe18075ac6d6795d8b41d3`
plus the captured working tree. The source manifest SHA-256 was
`499c80708e4e75a2ff72a4b39055fdfc00dd846481bba918b2472f0de143d7ab`;
the local node binary SHA-256 was
`1856476b84e37d2a3c2653b4f6aca547b3e4cc5d3b5e79b2a373bcb37ef73551`.
These identify development inputs, not signed release provenance.

The native arm64 container build used a dedicated builder capped at one CPU
and 4 GiB RAM without swap, after checking at least 12 GiB available RAM.
Runtime was limited to 0.75 CPU, 768 MiB, and 128 processes, with a read-only
root filesystem, dropped capabilities, no new privileges, and a bounded tmpfs.

Verified behavior:

- Authenticated pairing over the Tailnet and explicit selection of Atmos as
  the replica provider.
- Backup of 64 MiB of incompressible random data, a small text file, and an
  empty directory; pause and resume of the exact same job.
- Completed-copy verification, then restart of only the drill's provider
  container with the same durable transport identity.
- Deletion of only the temporary source and local chunk cache, followed by
  a provider-only restore with exact hashes, text, and empty-directory checks.
- Cleanup of the drill's local process/tunnel, temporary directories,
  container, dedicated builder, volume, generated image, and newly pulled
  unused builder image. Existing container and image IDs remained present;
  a follow-up read-only inspection found no drill resources left on Atmos.

The backup including pause/resume took 26.41 seconds. Provider-only restore
took 24.93 seconds, or 2.57 MiB/s for the random payload. These measurements
include this network path and a deliberately capped development container;
they are not a throughput guarantee or a replacement for local performance gates.

The first attempt exposed SFTP staging changing an owner-only token to mode
0644. The node correctly refused it. The harness now explicitly sets and
checks 0600 permissions after transfer; that attempt's resources were also
removed before retrying.

This drill preserves the owner's identity and metadata while losing its
source and local data chunks. It does **not** prove recovery after loss of the
entire owner device, automatic two-way synchronization, or native mobile UI
behavior. Those remain separate completion gates.

The later [complete owner-loss drill](atmos-owner-loss-2026-09-07.md) separately
passed deletion of the entire owner state, recovery from exported private
files, automatic catalog import and provider-only restore.

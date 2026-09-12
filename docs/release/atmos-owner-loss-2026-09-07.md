# Complete owner-loss recovery through Atmos Docker

Status: passed as development evidence; repeat on the final release artifact.
Atlas is offline. The owner accepted Docker validation as its completion path;
this test does not claim an installation on physical Atlas hardware.

The second `scripts/test-remote-drill.sh --owner-loss` attempt passed using
generated temporary content and the authorized Ubuntu Atmos server. The source
manifest exactly matches published checkpoint
`18397969fd9d67e96a75aca85e072db747703f9d`, tree
`c9d37326c051895778498ce6558c4752b953c0f7`. The script recorded its starting base
`45e579d99f930bedde79b07818897fb50f3220d6` plus the four reviewed documentation
and diagnostic-script changes subsequently published in that checkpoint.

- Source manifest SHA-256:
  `5cdbc1d28332af55bf46a005289362bc7afd4764596b658614a4367376f307e8`.
- Local debug node SHA-256:
  `ea2e2ddee535241cd381eb0a5722e88f3ffbab121e2a3b2e66f3b865be34e9af`.
- Generated 64 MiB incompressible source SHA-256:
  `a9b7d1d755765b555d8b1d45438d34deadede1b5ef67ba0d84998d31764b62c9`.

The source and binary identifiers are development evidence, not signed release
provenance. The local node is a debug build; the remote image uses the release
Docker build.

## Verified sequence

1. Pair two disposable nodes and explicitly select only the Atmos provider.
2. Back up a random 64 MiB file, text file and empty directory. Pause and resume
   the same backup job; require one selected provider, zero degraded failures,
   and that provider's verified availability to be exactly `complete`.
3. Restart only the owned provider container and prove its durable identity is
   unchanged. Export the protected recovery kit/key pair into private files.
4. Stop the original local node and remove its entire state, source, local
   encryption key, token and log. Generate fresh local key protection and API
   credentials, then start recovery directly from the exported private files.
5. Require automatic catalog import from exactly the selected provider, the
   original owner identity, exactly the expected backup/snapshot, no recovery
   failures, and no retained local data chunks before restoring.
6. Restore from the provider and compare exact file and directory inventories,
   text bytes, and the complete random-file SHA-256.

Backup including pause/resume took **27.16 seconds**. Owner-loss restore took
**26.80 seconds**, or **2.39 MiB/s** for the random payload. This measures this
network path under the deliberate container limits; it is not a WAN throughput
guarantee or the separate local performance gate.

## Isolation and cleanup

The dedicated builder was capped at one CPU and 4 GiB RAM without swap, with
at least 12 GiB available RAM checked before creation. A read-only sample of
that builder measured 100.73% of one CPU, 975.3 MiB of 4 GiB, and 37 processes.
The runtime was capped at 0.75 CPU, 768 MiB and 128 processes, with a read-only
root, dropped capabilities and bounded tmpfs. Only disposable mounts and
unused selected ports were used.

The command exited successfully after cleanup. Its checks confirmed all 21
pre-existing container IDs and 35 pre-existing image IDs remained present.
Follow-up read-only checks confirmed the exact owned container, builder,
BuildKit container and tagged image were absent, and the local fixture was
removed. Shared BuildKit base-image cache was intentionally retained. No
existing service, host setting, user folder or container configuration was
changed by the test.

## Earlier failure and limits

The first full owner-loss attempt failed during replication after pause/resume,
before recovery-file export or owner-state loss. Its aggregate result did not
identify whether chunks or the recovery catalog failed. Cleanup succeeded.
The cause remains unconfirmed; this later pass does not retroactively make
that attempt successful.

The harness now emits bounded redacted receipt counts and fixed error
categories on such a failure, plus a bounded provider-verification attempt.
Fixtures prove no secret/identifier leakage, nofollow/nonblocking private-file
reads, strict size bounds, and preservation of the original failing exit.

Separately, three local two-node HTTP/QUIC repetitions using continuous BLAKE3
XOF data passed exact same-job pause/resume. Each transferred 121 encrypted
objects totaling 67,110,800 bytes, with one catalog acknowledgement, zero
failures, and complete provider availability. Those local runs isolate the
protocol path but do not establish WAN reliability.

This evidence proves full owner-loss backup recovery for these inputs. It does
not prove automatic two-way folder sync, final package install/upgrade, native
large-catalog memory use, or completion of the entire project.

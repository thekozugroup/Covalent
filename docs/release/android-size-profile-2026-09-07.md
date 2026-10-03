# Android native size and host transfer check

Checkpoint: `e5a622c686c25bf41ef6561d28b4bbeb99ca9023`.
The hosted PR merge `df1597e0dc2bd557707cfa4176a20c2699c9fca5`
has the identical source tree. This is measured checkpoint evidence, not final
release or Android-device performance acceptance.

## Native library sizes

Android-only Cargo overrides set `opt-level="s"` for `covalent-node` and
`covalent-android-jni`. Core, cryptography and networking dependency packages
retain ordinary release optimization; generic code instantiated in the node
follows its optimization level. Fat LTO, one codegen unit, stripping and panic
unwinding remain enabled. The JNI boundary still uses `catch_unwind`.

Both hosted Android lanes independently produced the same sizes using Rust
1.97.1, cargo-ndk 4.1.2 and NDK 27.1.12297006:

| ABI | Previous recovery checkpoint `9e52876` | This checkpoint | Reduction |
| --- | ---: | ---: | ---: |
| arm64-v8a | 9,612,024 bytes | 8,384,224 bytes | 1,227,800 bytes (12.77%) |
| x86_64 | 11,483,168 bytes | 9,918,032 bytes | 1,565,136 bytes (13.63%) |

Both pass the unchanged 4,194,304-byte floor and 11,141,120-byte ceiling.
The comparison includes the QUIC shutdown correction in this checkpoint;
it is a before/after artifact measurement, not an otherwise identical codegen
experiment. Evidence: [Android foundation job](https://github.com/thekozugroup/Covalent/actions/runs/34177689199/job/101910267892)
and [Android device job](https://github.com/thekozugroup/Covalent/actions/runs/34177689199/job/101910267928).
Both lanes subsequently stopped on one user-copy JVM assertion, before
instrumented tests; passing native sizes does not make either lane green.

## Host transfer comparison

On an arm64 Apple M5 Max with macOS 26.6.2 and 64 GiB RAM, the exact same
source was built once with ordinary release settings and once with the node
size override. Copied test executables ran the existing 64 MiB real-QUIC
backup/verify/source-loss-restore scenario in alternating order, two samples
per profile. No compiler process was found in the activity snapshot before
any run. Other system activity was not excluded.

| Profile | Backup throughput samples | Mean wall time | Largest RSS growth |
| --- | --- | ---: | ---: |
| Ordinary release | 26.215, 30.006 MiB/s | 4.54 s | 97,648,640 bytes |
| Node size override | 27.750, 27.975 MiB/s | 4.58 s | 118,669,312 bytes |

All four runs verified exact restored bytes and the empty directory, with no
transport failures. Each passed the existing 600-second runtime, 192 MiB RSS
growth and 224 MiB disk-use bounds. Mean backup throughput differed by -0.88%;
two samples do not establish a stable regression or improvement. The 1 GiB
mode's 20 MiB/s assertion was not executed in this 64 MiB comparison.

The size-profile macOS test executable grew from 6,612,928 to 7,513,920 bytes.
That executable is neither a JNI library nor the macOS app/helper; the Android
build overrides do not change macOS release packaging. This difference is why
the actual Android measurements above, rather than host binary size, decide
the Android budget result.

Commands, hashes, raw measurements and copied executables are retained locally
under ignored `artifacts/validation-2026-09-07/perf-size-profile/`. Repeat native
sizes on the final revision, and measure Android runtime behavior on its real
artifact before release acceptance.

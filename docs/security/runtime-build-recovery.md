# Runtime build recovery — October 21, 2026

The reviewed runtime stopped building after Alpine published musl 1.2.5-r24.
Dependency resolution installed that signed revision while the source lock
still required r23. The lock rejected the difference as intended.

Both musl and musl-utils now require r24 explicitly. Their installed package
records identify aports commit `fd05fc094264bce4fb76efc6c7f61ba4abfcb609`.
The [complete upstream recipe](https://git.alpinelinux.org/aports/tree/main/musl?id=fd05fc094264bce4fb76efc6c7f61ba4abfcb609)
adds the regex overflow fix from upstream musl commit
`7813e9119eb5fe681b6750b8a033a0490bca0941`. Its existing iconv patch is renamed
without changing its bytes. The source archive remains the same musl 1.2.5
release. All 24 recipe files, original modes, Git blobs, the full directory
tree, SHA256, sizes and declared SHA512 checksums are locked and verified.

Alpine's primary server had the revision before its GitHub mirror. The complete
recipe is retained under `packaging/docker/alpine/musl-1.2.5-r24/aport/` to avoid
depending on mirror propagation or primary-server rate limits. The collector
admits only the exact primary-server file URL at the locked commit. Alternate
commits, files, origins, queries, fragments, protocols, ports, credentials and
hosts fail. Download redirects remain refused. Recipes are never executed;
runtime packages still come from Alpine with signature verification.

The OpenAPI gate also treated the retained first failed address test log as
an undeclared client. That single path now requires its original SHA256, using
the existing frozen-evidence mechanism. Negative fixtures reject altered log
bytes, a removed API prefix and an adjacent unlisted log. No directory or file
extension is exempted, and executable client coverage remains unchanged.

## Verification boundaries

Evidence is retained under `artifacts/evidence/runtime-build-recovery/`.
The export Dockerfile is the current production Dockerfile with a final scratch
stage that exports the actual `runtime-patched` package database, actual Caddy
builder database, copied CA and Alpine distribution bundle. This executes the
real package installation, BusyBox build/regressions and source collector. It
does not build or accept the full Rust/worker/Caddy application image.

The original Docker VM was full; both initial exports failed before compiling.
Their receipts and logs remain. Verification moved to an owned, separate Colima
profile with four CPUs, 4 GiB memory and a 30 GiB data disk. The default context,
existing containers and build cache were preserved. Builds are sequential.
Review pins must be regenerated from actual exported files, then checked by the
independent verifier against the installed database and source lock.

Complete image functional, payload, source, notice, size and vulnerability
checks remain separate release gates. Pi/phone checks are deferred at the
user's request. This change does not update the APK, Pi runtime or drive.

## Other hosted failures retained

Source `1a2874e`, run 39658372282, fails its new macOS address gate before either
endpoint moves: generation 1 remains preparing with no worker folders/health
observed. It times out in 76.11 seconds. The paired marker and cleanup complete;
the first transfer and address transitions do not. The local passing address
test does not close this failure or the earlier intermittent Android stall.

A controlled local version probe leaves the original worker unchanged. The
original linker-signed worker and an owned copy signed with hardened runtime
alone both return success with identical output. An owned copy signed with the
app's exact sandbox-inheritance entitlements traps from the same ordinary
shell/Python parent. This supports investigating the CI launch context: the
packaged helpers require sandbox inheritance, while Cargo's test runner is an
ordinary executable. It does not prove the hosted failure's cause or validate
a corrected sandboxed parent. No signature requirement is removed.

Source `9330537`, run 39651980156, artifact 12372048269, retains the real permission
failure receipt. The archive digest verifies as
`4aa171da3613b58ca46e5060ae68d3e0a0cd18ca85392777f9c02c4a4375fbec`.
The receipt correlates live and DropBox system_server SIGABRT records and a
mapper.ranchu.so stack. The 62-byte abort-message digest equals the known
`Assertion failed: !rcEnc->featureInfo()->hasReadColorBufferDma` string.
The fatal signal precedes the failed Permissions tap; the replacement framework
starts afterward. The tap and permission-restoration command report missing
services. All three synthetic identity digests remain unchanged at failure.
These observations identify the known emulator abort, not its initiating
trigger or a passing permission-recovery test. No rerun or emulator flag change
is used to conceal either failure.

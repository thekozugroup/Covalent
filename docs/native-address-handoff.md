# Paired manual sync after endpoint changes

The real maintained rclone worker now has an explicit two-node regression gate
for changing both the encrypted peer port and worker port. It verifies three
acknowledged manual transfers, a receiver move, a sender move initiated from the
receiver, and reopening both nodes with retained identities and link settings.
The old ports are occupied by inert test sockets so port reuse cannot conceal a
missing route update. Transfers begin directly after authenticated refresh.

The test preserves source bytes, manual cadence, disabled source-deletion
propagation and disabled restoration of receiver deletions. Exact address and
manual-run request replays survive reopening without creating generation four.
All nodes are registered for cleanup, including failed scenarios. A single
240-second scenario deadline bounds the run. Synthetic temporary folders and
workers are removed after shutdown; personal devices and accounts are untouched.

The macOS bundle CI lane runs this test against its just-built packaged worker
and guardian. It therefore cannot pass merely because the default Rust suite
skips tests requiring explicit engine executables.

## Retained results

Evidence is under `artifacts/evidence/native-address-handoff/`.

- The first test fails in 146.695s wrapper / 140.24s test time: its ordinary
  `post_ok` helper rejects an explicit HTTP 503 `folder_sync_busy` address response.
  This is a test-contract failure, not evidence identifying the old Android stall.
- The revised helper keeps the exact request, retries only HTTP 503 with code
  `folder_sync_busy`, and retains its original 60-second deadline. Other errors
  fail immediately. Phase markers identify the transition being checked.
- That revised test passes in 141.751s wrapper / 136.38s test time. All six address
  calls return without a busy response, so this pass does not exercise the new
  busy branch. Both device/certificate identities and generation three survive
  reopening. The result and cleanup markers are retained.
- Clippy passes with warnings denied in 14.527s. Foundation validation passes
  in 43.849s. Formatting and whitespace checks pass. No production code changed.

The worker is the reviewed macOS arm64 build with SHA256
`07cb75e9292c3c3a08322f4e34a6b1e6f91599b43ae917ae64f6d13bfb864922`.
The guardian is freshly compiled from the unchanged reviewed source. The manifest
binds source, executable hashes and verification records. Executables remain
local ignored artifacts.

This desktop-engine regression does not reproduce or resolve the earlier
intermittent Android post-address transfer timeout. SAF, Android background
execution, physical roaming, and real Pi/phone acceptance remain separate gates.

## Hosted checks discovered during this milestone

The original source 9330537 CI39651980156 is now terminal failed: seven software
lanes, all 86 baseline Android tests and its SAF journey pass. Permission recovery
fails on `adb shell input tap` exit 20 after an emulator invalid-color-buffer
message. The log alone does not establish the exact native assertion or cause.

Source 4a89c2f CI39655613903 fails the two container lanes and the Android fixture
build before Android device tests. Both container logs show musl upgrading from
1.2.5-r23 to1.2.5-r24, then the installed-runtime evidence comparison refuses the
unreviewed package difference. Other differences still require inspection.
No lock, scan or device gate is bypassed; no manual rerun is requested.

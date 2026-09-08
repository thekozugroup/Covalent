# Experimental Syncthing Android executable proof

This standalone application is a feasibility gate. It is not part of Covalent's
product package, JNI bridge, release, or synchronization protocol. Its application ID is
`life.michaelwong.covalent.engineproof`.

It cross-builds the exact official Syncthing v2.1.3 commit
`946e2b83a1f6c6ae119427c09e0a5802940b82ff` with Go 1.26.7, NDK
27.1.12297006, API 26, CGO, `-mod=readonly`, and `noupgrade`. Both `arm64-v8a`
and `x86_64` helpers are packaged as `libsyncthing.so`. The reviewed guardian is
cross-compiled for the same ABIs as `libengineguardian.so`, pinned by its source hash,
and limited to 8-256 KiB. The Gradle project copies the
repository's pinned Gradle 9.7.1 wrapper, AGP 9.2.1, SDK 37, and dependency-verification
metadata. Legacy native packaging is deliberate: the test needs a package-manager-extracted
immutable executable, because Android forbids executing a writable app-data copy.

The pinned Android dependency `github.com/wlynxg/anet` v0.0.5 uses Go's internal
network-interface zone cache. Its [upstream build instructions](https://github.com/wlynxg/anet#how-to-build-with-go-1230-or-later)
require `-checklinkname=0` with Go 1.23 and later. The experiment uses that flag:
the initial cross-link without it failed at `net.zoneCache` on Go 1.26.7. This is
an explicit dependency on a private Go API, not proof of compatibility with a
future compiler. The source/compiler pins and actual Android runtime gate stay
required; no older compiler is substituted.

## What the API-37 tests prove

The original x86_64 instrumentation test checks:

* the installed helper is an executable, app-nonwritable file under `nativeLibraryDir`,
  outside writable `dataDir`, with the exact hash and size emitted by the pinned build;
* `--version` reports v2.1.3 for Android;
* identity generation receives no secret and writes only inside credential-encrypted
  `noBackupFilesDir`;
* before serving, the test writes a mode-0600 config with no active folder, no protocol
  listener, public/global or local discovery, relays, NAT, STUN, usage/crash reporting,
  browser start, or automatic upgrade; the only listener is the GUI/API on a random
  `127.0.0.1` port;
* the API key exists in the config only and is absent from child argv, environment and
  retained logs; the only Syncthing environment setting is the non-secret
  `STMONITORED=1`, which selects the tagged direct engine path instead of its monitor/child
  process; missing-key API access is rejected while the exact key succeeds;
* authenticated shutdown exits the exact directly-owned process with zero status and
  immediately releases the loopback port and data-directory lock; and
* a cold restart retains the same Syncthing device identity/certificate and the offline
  config, then shuts down cleanly again.

The second test keeps the original test intact and adds the reviewed guardian boundary:

* the installed guardian is a separately hashed, executable, app-nonwritable file under
  `nativeLibraryDir`;
* a bounded `/proc` observation requires exactly one installed guardian child and exactly
  one direct installed Syncthing worker beneath it while the authenticated API is live;
* closing the guardian's Java-owned stdin lifeline stops and reaps the worker, releases the
  API socket/data lock, and permits a same-identity restart;
* forcibly terminating the exact Java-owned guardian `Process` requires Linux
  `PR_SET_PDEATHSIG` to terminate that exact worker; assertion cleanup closes the owned
  lifeline first, uses the private authenticated API if the worker remains, and signals the
  owned guardian handle only after the API endpoint is released, never an observed numeric
  PID; bounded `/proc` observation must find no process using the exact installed helper
  executable before private fixture deletion; and
* the database, socket, certificate and engine identity reopen after both owner-lifeline
  and guardian-death paths before a final authenticated shutdown.

The native build rejects a dirty/wrong upstream checkout, wrong Go or NDK, a helper outside
5-64 MiB, a guardian outside 8-256 KiB, non-PIE ELF, PT_LOAD alignment below 16 KiB,
text relocations, missing guardian RELRO/NOW/non-executable-stack hardening, and unexpected
dynamic libraries. The guardian link uses `--as-needed`, retains bounded per-ABI dynamic and
symbol-table reports, and still permits only `libc.so`. Android documents `libdl.so` as a
[separate NDK platform library](https://developer.android.com/ndk/guides/stable_apis#c_library),
but the guardian source calls no dynamic-linker API, so the audit does not admit it without
link evidence that it is required. Instrumentation bodies and logs are capped at 128 KiB,
startup at 30 seconds, shutdown at 20 seconds, and the entire device invocation at 180 seconds.

## Build

No tool installer is included. Supply an existing SDK/NDK and an exact clean checkout:

```sh
export ANDROID_HOME=/absolute/path/to/android-sdk
export COVALENT_ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.1.12297006"
export SYNCTHING_SOURCE_DIR=/absolute/path/to/syncthing-v2.1.3
test "$(git -C "$SYNCTHING_SOURCE_DIR" rev-parse HEAD)" = \
  946e2b83a1f6c6ae119427c09e0a5802940b82ff
go version  # must say go1.26.7
./gradlew --no-daemon --dependency-verification=strict \
  -PsyncthingSourceDir="$SYNCTHING_SOURCE_DIR" \
  :app:assembleDebug :app:assembleDebugAndroidTest
```

The build emits separate per-ABI hashes and sizes in
`app/build/generated/syncthingProof/assets/syncthing-sha256.txt` and
`engine-guardian-sha256.txt`.

## API-37 run

With an already-running x86_64 API-37 emulator:

```sh
ANDROID_SERIAL=emulator-5570 ./run-api37-proof.sh
```

Use `--prebuilt` only when the two APKs were built from the exact source in the same job.
The script installs and removes only the experimental package and caps instrumentation at
three minutes.

Hosted execution uses the repository's existing API-37 guest preparation and
stability checks. The standalone harness's first instrumented run omitted that
preparation and Android aborted it with `System has crashed`; it produced no
test result. The runner now requires unchanged `surfaceflinger` and
`system_server` PIDs across instrumentation and emits bounded, credential-redacted
guest diagnostics on failure. Guest graphics preparation is emulator-specific;
this proof neither exercises a launcher nor validates screen capture.

The corrected original one-test gate passed independently at commit `5716068` in hosted
run `34224544802` before the guardian test was added. The runner now requires the exact two
method names, one start and one success status for each, `numtests=2`, `OK (2 tests)`, the
successful final instrumentation code, and stable framework PIDs; the guardian path remains
unproven until that expanded hosted run passes.

The first expanded build run (`34229120120`) stopped at the unchanged guardian dependency
allowlist because the arm64 link retained `libdl.so`; no expanded instrumentation ran. The
next build asks LLD to retain dynamic libraries only when needed and publishes the bounded
dynamic/symbol reports before applying the same `libc.so`-only gate. Whether this removes
`libdl.so` for both ABIs remains a hosted NDK result, not a local claim.

The first prepared run timed out before instrumentation because the shared
readiness helper still targeted the production app's Compose activity. The
workflow now supplies its supported package/component overrides for this
experimental APK and its declared, non-direct-boot platform Activity. All
credential-unlock, service latency, install and six-sample stability requirements
remain in force. This is a harness correction, not a runtime pass.

## Deliberate limits

This does not prove API-26 or arm64 runtime execution, foreground-service survival, Doze,
reboot, managed `DocumentsProvider`, editor races, existing Pictures/Downloads access,
two-device convergence, Syncthing release licensing, or compatibility with Covalent's
signed history/retention/freeze/apply contracts. The arm64 and API-26 evidence here is a
cross-build and ELF audit only.

The guardian enumerates `/proc/self/fd` before fork and applies a 16,384-open-descriptor
bound. The API-37 test is required to prove that enumeration, child `fork`/`exec`, and
`PR_SET_PDEATHSIG` are permitted by the actual Android guest. Numeric PIDs from `/proc` are
observational bindings only and are never used for cleanup.

Syncthing also supports an absolute filesystem Unix-domain GUI/API socket. A private socket
under a mode-0700 directory may reduce local TCP exposure, but Android client compatibility,
pathname limits, lifecycle cleanup and authorization require a separate proof. This harness
keeps the bounded loopback-TCP test so it does not enlarge the present gate.

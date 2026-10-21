# Android test display readiness

An open notification shade prevents the unchanged onboarding Back test from
receiving focus on the owned API37 ARM64 emulator. Before tests begin, the gate
now wakes the emulator, dismisses its lock screen, closes system panels and
verifies an interactive display. This is test setup; later focus loss still
fails the original test.

Hosted8b3cb32 run39681087839 failed one of86 baseline tests at
`Espresso.pressBack`, with all144 complete current-focus observations classified
as unrecognized. Its raw titles were intentionally discarded. On the local
emulator, an expanded notification shade reproduces the same exception at the
same source line, and the old recorder similarly reports unrecognized focus
throughout. This establishes a local failure cause and an observation gap; it
does not establish what owned focus on the hosted guest.

`WindowState.toString()` uses a window's title, which need not contain an
activity component ([AOSP source](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/services/core/java/com/android/server/wm/WindowState.java)).
The recorder now recognizes only the exact public `NotificationShade` and
`StatusBar` descriptors in addition to its existing component categories. It
also records booleans from the scoped `KeyguardServiceDelegate` block. Arbitrary
titles, package/class names, window dumps and command output remain discarded.
An incomplete read still supplies no partial state; conflicting fields stay
ambiguous. These categories describe observed titles, not authenticated process
identity.

`prepare-android-test-display.py` admits only the existing pinned5570/5622 AVD
identities. Before any instrumentation starts, it executes wake,
`wm dismiss-keyguard`, and `cmd statusbar collapse` once each. Every client has
a byte cap and deadline. A nonzero exit, timeout, incomplete read, changed boot,
unknown focus owner, pending/occluded/showing keyguard, dream, sleeping screen or
frozen display rejects readiness. Two consecutive complete observations must
agree on a known foreground category and focused-app category, with display0
awake and fully on. Sampling is capped at40 reads and a10-second admission
window; an in-flight read can take up to2 seconds to stop. Each setup/scalar
client has a3-second deadline. No test runs if admission fails.

The gate invokes preparation once immediately before the baseline, after its
owned fixture installation. The old unchecked workflow keyguard command is
removed. The existing baseline/SAF commands, Back call, JUnit verdict parser,
test set and test deadlines remain unchanged. There is no recovery action
during instrumentation and no new test retry.

Evidence is bound by `artifacts/evidence/android-window-titles/manifest.json`:

- The deliberately expanded-shade case fails in20.592s host time at the same
  Back line; the original named-test validator rejects it. All33 observations
  are unrecognized with the old recorder. That failure is retained.
- The new preparation recognizes that shade and establishes launcher focus.
  The unchanged test passes in7.503s with the first policy version.
- A POWER toggle did not establish sleeping state in the immediate observation;
  that observation and its preparation receipt are retained and excluded from
  the sleep claim. A subsequent deterministic sleep fixture observes all three
  awake/screen-on flags false. The final-source preparation wakes that fixture
  and establishes readiness. The final unchanged Back test passes in5.964s host
  time with9 complete observations, stopped clients/sampler, and unchanged
  boot/framework PID/start-time pairs. Its named-test validator accepts it.
- All19 recorder and12 preparation tests pass on macOS and isolated Linux. They
  cover exact-title privacy, scoped keyguard state, conflicts, unknown/locked
  states, changed identities, command failure, evidence links, cancellation,
  consecutive readiness and both sample/deadline bounds. The existing35 reader
  checks pass on both hosts. The unchanged verdict mutation battery still
  requires all87 source tests; foundation and shell syntax pass.
- All214 relevant APK inputs match the prior build. Both installed debug APKs
  are read back before/after testing with identical hashes; neither is rebuilt
  or installed. Only the owned emulator/verification VM are stopped, retaining
  their disks; the default Docker VM/context and other emulators are preserved.

Full hosted baseline, SAF and permission acceptance remain open, as do the
separate native mapper abort and intermittent transfer stall. No production app,
Pi ROM, account, pairing identity or original media changes in this milestone.
Physical Pi/Solstice checks, production signing and release acceptance remain
separate gates.

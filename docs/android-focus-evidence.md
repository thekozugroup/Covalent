# Android instrumentation focus evidence

The unchanged onboarding test intermittently fails at Espresso Back because its
app root lacks window focus. Hosted source6944b16 run39670174310 fails one of86
baseline tests; the SAF and permission journeys are not reached. Its artifact
is retained and ZIP-verified. A passing isolated ARM64 test does not explain
that hosted x86_64 failure or the separate native mapper abort.

Both existing device-gate phases now run through
`scripts/android_instrumentation_focus.py`. It launches the same instrumentation
command once, preserving arguments, output and exit status. The existing named
test/result validator still judges JUnit success, including failed tests whose
ADB client exits0. No Back implementation, timeout, renderer, app runtime,
permission or test expectation changes.

One host sampler reads `dumpsys window` every500ms, with no overlapping reads.
Each read has a256KiB cap and2-second deadline, using the existing bounded client
reader. Only fixed owner categories, known boolean state and a bounded display
number enter metadata. Window titles, arbitrary packages/classes, accounts,
paths, tokens, raw dumps and stack text are discarded. An incomplete read cannot
supply partial focus attribution; missing/conflicting fields remain explicit.

The sampler keeps256 observations. It tails the existing phase log in bounded
chunks, recognizes only numeric test ordinals/statuses and the fixed focus
exception type, and freezes the last32 observations at each of at most16 failed
statuses. Later tests cannot erase that failure's preceding observations.
Dropped counts, unavailable reads and collector errors are explicit. Timestamps
are host observation intervals, not the exact time Back was delivered.

Only the pinned CI emulator5570/Covalent_API_37 and the owned local fixture5622/
Interlink_Back_Focus_API37 are admitted. Output must be new. Cleanup stops the
owned clients and joins the sampler; cancellation dispatches no replacement
instrumentation. A diagnostic failure cannot replace the original child's
result. This recorder supplies evidence, not a functional test exemption.

Validation is source-bound in `artifacts/evidence/android-focus-observer/`:

- All16 observer tests pass on macOS and isolated Linux. They cover privacy,
  conflicting/missing fields, bounded history, split/overlong logs, incomplete
  reads, completed ordinal reset, admission and unchanged child results.
- Existing bounded-reader tests pass35 checks. The unchanged instrumentation
  result mutation battery passes with all87 named source tests still required.
  Foundation checks and shell syntax pass.
- Three real local subprocess cases preserve JUnit-failure exit0 and transport
  exit17. SIGTERM returns143, stops the owned original client and joins the
  sampler. Each case starts instrumentation once. The unchanged validator
  rejects the synthetic JUnit failure. These are hermetic fixtures, not Android
  failure reproductions.
- On the owned API37 ARM64 emulator, controlled launcher/app/Settings foreground
  changes produce the expected categories. The unchanged original Back test
  passes with the final recorder source in5.811s and10 complete observations.
  Boot and framework PID/start-time pairs stay unchanged; all read clients and
  samplers stop. The installed debug APKs retain their previous exact hashes;
  all214 relevant APK source inputs match the prior build. No APK is rebuilt or
  installed. The first passing capture predates the ordinal-reset guard and
  retains its separate source hash; final-run/ is the current-source proof.

The owned emulator is stopped with its disk retained. Physical devices and
original media are untouched. Full hosted baseline/SAF/permission acceptance,
the original focus-loss cause, intermittent address recovery, production signing
and Pi image/release acceptance remain open.

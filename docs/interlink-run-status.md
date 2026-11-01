# Unfinished receiving runs

A source can disconnect while its saved run still says it is running. After
that run's deadline, receiving cards now show **Waiting for source** and explain
that files and progress are kept. They no longer imply a current transfer or
repeat the generic unknown-connection message beneath the explanation.

This is an uncertainty display. It does not finish, cancel, expire, retry or
rewrite the source-owned run, its signed state or its generation. It grants no
cleanup authority and does not change the Pi's update-idle barrier.

A fresh connected source can still report an active run after the local clock's
deadline. Fresh local copy/scanning/error observations retain their existing
priority. An observed local copy still shows Transfer running. Outgoing runs,
missing deadlines and completed/incomplete/interrupted/cancelled history retain
their previous behavior. Stale connection observations cannot establish contact.

## Verification

The normal companion debug APK retains both native ABIs, its normal Application
and all native producers. JVM coverage includes deadline boundaries, missing and
stale connections, a connected source, live local copy, errors, outgoing runs,
missing deadlines and unchanged terminal/saved records. The app's full 269 JVM
tests pass, alongside companion debug lint and native package checks.
All24 registered companion component/owner/background/notification cases pass
on the owned API36 ARM64 16KiB guest after real Get started and actual Android
notification consent. The new receiving-status transition also passes separately
at normal text/light and 200% text/dark. Framework identities and boot remain
unchanged; font/night settings are restored and animation settings are retained.

The first broader run lacked host opt-in and the next lacked notification
consent. Their original failures remain in the audit. Completing those real UI
prerequisites makes the unchanged tests pass; no timeout or assertion is relaxed.

`InterlinkRunStatusInstrumentedTest` renders the actual shared receiving card in
the native Expressive theme: uncertainty/explanation visible, Running/Completed
and the redundant generic message absent, then fresh copy observation restores
Running without claiming completion. It is included in the existing strict
API37 companion suite registry; local API36 validation does not replace that lane.

The receiving-link previews use labelled example data. They exercise this shared
card in Interlink; they do not reconfigure the owner's phone-to-Pi Pictures link.
Native captures and exact package/device/check/cleanup scope are recorded in
the corresponding private appliance audit. This does not establish a production
companion signature, real phone update, physical import, cloud backup or ROM
upgrade/recovery acceptance.

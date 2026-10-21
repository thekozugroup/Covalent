# Pair before choosing a photo source

The dedicated photo companion now shows pairing first. Source-folder selection,
the share label and outgoing-link controls appear after the node reports a
retained, confirmed peer. This uses the existing status snapshot, so it reflects
saved pairing rather than a separate onboarding flag. A paired device can be
offline: the peer list comes from retained trust and does not require a live
connection.

The ordinary Covalent screen keeps its existing behavior. Incoming destination
selection and lost-folder-access repair keep their separate visibility rule;
startup and worker-retry controls remain reachable. No engine, trust, API,
deletion, file-selection or authentication contract changes.

The preceding milestone's exact app APK passes the full synthetic native SAF
journey; that evidence remains separate. This UI-only companion change requires
its own build, lint and installed-screen checks. It does not convert the earlier
intermittent address stall into a fix or establish hardware/cloud acceptance.

The new debug APK passes build/lint and an actual in-place emulator update with
the same package UID. Its unpaired screen shows pairing controls and hides the
source chooser, share label and premature share guidance. The original setup
receipt, boot and framework identities stay unchanged. Paired/offline and repair
branches are reviewed in source only in this smoke; no new paired-transfer or
large-font acceptance is claimed. See
`artifacts/evidence/interlink-progressive-setup/`.

The package also passes 16-KiB ZIP alignment and signature verification with the
existing debug certificate. All eight packaged native libraries, including the
six owned helpers, and native digest/notice assets remain byte-identical to the
previously exercised Covalent APK. The route gate and full foundation validation
pass. This is not production signing or a new full companion sync journey.

# Frozen client source evidence

The route gate recognizes three exact historical Android source snapshots as
nonexecuted regression evidence. Each path has a pinned SHA-256. Changed or
unreadable bytes fail, including replacement content with no API prefix. No
folder or extension is exempt. Adjacent unknown clients still fail discovery.
Current application trees retain their method/path extraction and routing checks.

This repairs the shared-foundation failure in source 0564034 run 39635433377,
job 119344484472. The original failure and APK results remain unchanged.

The live route gate passes 54 router/spec operations, 397 references and 50
client operations from 120 verb-checked call sites across seven trees. Existing
route fixtures and three new negative cases pass. The full foundation script
passes on macOS; its local raw log and hash are recorded in
artifacts/evidence/frozen-client-evidence/result.json. This does not establish
hosted Linux, Android address-change recovery or release acceptance.

This source is published separately on `codex/frozen-client-evidence` while the
original CI's Android and container jobs continue. It changes no APK or image.
The primary PR branch will be promoted only after the original run is terminal.

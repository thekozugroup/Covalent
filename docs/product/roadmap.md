# Roadmap

Updated 2026-09-12. The owner narrowed the release to native one-way links.

1. Reuse the working pairing, native folder access, maintained worker, process supervision, and Docker packaging. Prove deletion behavior before selecting the final engine changes.
2. Implement one source with one or more destinations, independent links into a shared collection, and shared link settings.
3. Add the required deletion choices and manual, scheduled, and continuous operation.
4. Simplify native interfaces: Android floating actions and clear data/status; Mac HIG and per-link menu bar status.
5. Verify actual laptop/Atmos/Android journeys, failures, idle resources, and transfer performance. Clean temporary resources and publish accepted releases.

The [product requirements](requirements.md) define scope. The [completion ledger](../release/completion-progress.md) records verified progress.

macOS, Android, and Docker/Unraid are Tier 1 release targets. iOS and Windows are not supported; the retained iOS CI lane is informational.

The previous backup, bidirectional sync, and disaster-recovery roadmap remains in Git history at checkpoint 46. Those unfinished features no longer gate this release. Existing user files and identities remain protected. iOS and Windows stay out of scope. Docker is the accepted Unraid target while Atlas is offline.

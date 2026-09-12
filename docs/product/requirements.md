# Product requirements

Updated 2026-09-12 from the owner's explicit scope reduction and subsequent clarifications.

## Product promise

Covalent makes one-way file transfers easy to pair, configure, and monitor. A link has one source folder and one or more destinations. Destination files remain ordinary files. No hosted account or subscription is required.

Supported products: native macOS, native Android, and Docker for Unraid and other Docker hosts. Atlas is offline; Docker and isolated Atmos tests are the accepted server targets.

This contract supersedes the distributed encrypted-backup and bidirectional-sync release scope. Preserve existing user files and identities; never silently migrate their behavior. The previous source and evidence remain in Git history at checkpoint 46, commit c47003113e28b6e934a8ab4823040614fb07fb30.

## Links

- Choose a source, pair destinations, authorize their folders, then start.
- File content travels only from source to destinations. Destination changes never alter the source or another destination.
- Each link has one set of settings. Any authorized member can submit a change; the source commits and distributes a revision to all members. Offline edits stay visibly pending. Stale conflicting changes are not silently applied.
- Separate links have independent settings. Multiple family members can contribute to one collection without downloading its other contents.
- Each link owns a distinct subfolder of a shared collection. Identical camera filenames from different devices cannot overwrite each other. A link can remove only its own files.
- Removing a link stops transfers and preserves files.

## Deletion choices

The two choices are independent. Explain them before starting a link and in its settings.

| Setting | Default | User explanation |
| --- | --- | --- |
| Source file deleted | Keep destination copies | Deleting a source file leaves destination copies untouched. |
| Alternative | Delete destination copies too | Deleting a source file also deletes copies this link created at its destinations. Other links' files stay untouched. |
| Destination file deleted | Keep it deleted | Source and other destinations stay untouched. This destination does not download the file again until restoration is enabled. |
| Alternative | Restore from source | If the source file still exists, this destination downloads it again on the next transfer. |

A later source edit must not silently bypass keep-deleted behavior. Destructive setting changes require a review of their effects. Missing mounts, failed scans, lost permissions, and offline sources must never be treated as empty folders.

## Timing and status

Provide Run Now, Scheduled, and Continuous. Settings apply across the whole link. Android provides Wi-Fi and charging conditions and respects operating-system background limits; show waiting conditions without promising exact timing or unrestricted background work.

Scheduled/manual links stop their transfer worker when idle. Continuous links use file events and coalesce changes. Do not promise zero application or operating-system overhead.

Show per-destination status, meaningful progress, last success, pending settings, and actionable errors. Support pause, resume, retry, and file-preserving removal. An offline destination must not stop other destinations.

## Native interfaces

macOS follows Apple HIG: system typography, standard windows/settings, keyboard navigation, VoiceOver, and a menu bar item with a small status symbol and per-link status/actions. Use the supported menu bar API, not an invented third-party Control Center extension.

Android uses native Compose/Material controls. The reference is [Tomato](https://github.com/nsh07/Tomato); the owner prefers its floating action bar, typography, data visuals, and top bar. Adapt those ideas to transfers and accessibility. Inspect licensing before reusing code, fonts, or assets.

## Server and file safety

Docker is the default Unraid distribution. Use explicit mounts and a non-root service where supported. Live appdata requires an application-consistent source, such as a stopped application or snapshot. Ordinary file copying is not a guaranteed live-database backup. Copy readable boot files only through an explicit mount; do not claim full boot recovery without testing it. Add an Unraid plugin only for a demonstrated Docker limitation.

Retain authenticated pairing, encrypted transport, restricted folder access, path/symlink defenses, interruption recovery, bounded resources, and accessible errors. Reuse a maintained transfer engine. Do not build a distributed filesystem or multiple interchangeable engines.

## Completion

The [completion ledger](../release/completion-progress.md) defines ten complete acceptance checks. Existing foundation evidence is reusable but does not prove new link behavior.

Establish stability, then measure idle resources and representative transfer performance. Optimize measured problems. Put builds and test data in dedicated temporary directories. Retain compact evidence and release outputs; remove owned processes, fixtures, obsolete builds, and caches when finished. Never disturb unrelated Atmos services or user files.

The owner authorizes final GitHub releases after acceptance. Previously accepted personal-use signing scope remains. Do not label incomplete builds as complete releases. Report completion with one line each for Situation, Task, Action, and Result.

## Excluded from this release

Bidirectional editing, distributed conflict resolution, encrypted backup-provider pools, source-loss disaster recovery, historical version browsing, photo management, cloud services, alternative transfer backends, iOS, and Windows do not gate this product. Preserve existing data while removing these flows from the primary interface.

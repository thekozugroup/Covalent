# One-way folder links

Covalent sends files from one source to one or more destinations. Pairing alone
never shares files: select a source folder, invite a paired device, then choose
and accept its destination folder on that device. A destination does not send
its contents back to the source or to another destination.

This is the current development contract. The complete product is not yet
released; see the [acceptance ledger](../release/completion-progress.md) for
verified behavior and remaining failures. The original two-way design remains
in Git history at checkpoint 46. Existing two-way records keep their original
behavior and are explicitly labeled as legacy; they are not silently converted.

## Deletion choices

| Choice | Off by default | When enabled |
| --- | --- | --- |
| Delete destination copies when source files are deleted | Destination copies remain. | The source deletion also removes its destination copies. |
| Restore files deleted at a destination | Those files remain deleted there, including after source edits. | Covalent copies those files there again from the source. |

The apps explain and confirm the consequences of enabling either option.
Stopping a link preserves files already on each device. This is plain file
synchronization; a mirrored deletion is not a recoverable backup by itself.

## One setting for the whole link

Deletion choices and pause belong to the entire link. An authorized member can
request a change. The source commits its revision and sends it to every linked
destination. An offline source leaves the request visibly pending. Competing
edits require review; a stale request never silently overwrites a newer choice.
New destinations wait for the source's current settings before transferring.

Manual, scheduled and continuous operation use the link's shared settings.
Android also applies Wi-Fi and charging conditions within its background
execution limits. Batch workers stop between runs; status distinguishes waiting,
active transfer, paused, and errors. These runtime journeys pass the checks
recorded in the acceptance ledger; final native app and release checks remain.

## Collections and access

Independent links can contribute to one collection using a distinct child
folder for each source. Two sources must not manage the same files: separate
folders prevent collisions and keep one contributor's deletion policy from
removing another contributor's files. The source never receives the collection's
other contents. Create a separate link for each contributor and choose its
distinct child folder when accepting it on the destination.

Folder access is checked before transfer and after worker restarts. Lost
permissions and incomplete scans must not be treated as source deletions.
Covalent retains authenticated device consent, private engine control, verified
worker executables and restricted filesystem access. Idle engine status and raw
need counts do not establish that two folders contain identical files; files
intentionally left deleted can remain in those counts.

macOS uses native folder access grants and a menu bar view. Android uses its
available folder access and background execution facilities; force-stop and
platform scheduling restrictions must remain visible. Docker is the accepted
Unraid delivery path while Atlas is offline. Testing uses isolated temporary
folders on the laptop and Atmos.

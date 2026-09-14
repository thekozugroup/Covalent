# Create your first one-way link

Covalent wraps rclone for sending files one way: pair a
device, choose a source folder, then add one or more destinations. A source
never receives destination changes.

## Before you start

Use a current checkout. There is no published current release: `v0.1.0` is
historical, and `v0.2.0` has not been published. The development build paths are a
locally built Docker server, the personal-use Mac build, and the personal-use
Android debug APK.

- For an always-on destination, build Docker from this checkout. The current
  rclone path passes hosted Docker transfer checks on amd64 and arm64.
- Use the Mac app on Apple Silicon for the current source/package path. Native
  macOS HIG validation is still open.
- Android uses the system folder picker to grant access to each selected
  folder. It does not require all-files permission. Real API 37 transfers and
  permission-loss recovery pass; native Android acceptance tests pass.
  Mac HIG and VoiceOver acceptance, final package acceptance, and laptop–Atmos
  validation remain open.

Install guidance: [macOS](platform/macos.md),
[Android](platform/android.md), and
[Docker from this checkout](../packaging/docker/README.md#personal-use-from-this-checkout).

## Create the link

1. Pair the devices and check that their confirmation codes match.
2. On the source device, open **Links**. Android calls the page **Shared folders**
   and its creation action **Create link**. Choose the source folder and a paired
   destination. Choose when to transfer and review the deletion choices below
   before confirming the link.
3. On the receiving device, review the incoming link, choose its destination
   folder, and accept. Transfers follow the shared timing choice after both
   devices confirm the link and the source confirms its settings. Manual links
   wait for **Run Now**.
4. On the source, use **Add destination** for each additional paired device.
   Accept and choose a folder on each receiver. An offline destination does not
   stop the others.
5. For a family collection, create a separate link for each contributor and
   choose a separate child folder on the receiver, such as
   `Family photos/Sam's phone`. Choose these folders yourself; Covalent rejects
   overlapping link roots. Identical filenames stay separate, and each link
   can delete only its own files.

Use **Edit shared settings** to change the source and every destination for a
link. Authorized members can propose a change; it stays pending until the source
confirms it. Separate links have independent settings.

## Choose when to transfer

| Timing | What happens |
| --- | --- |
| **Manual** | Use **Run Now** on any linked device to request one transfer from the source to all destinations. The transfer worker stops when finished. |
| **Scheduled** | The source starts transfers at the chosen interval, from 15 minutes to one year. You can also use **Run Now**. The source displays the next due time. |
| **Continuous** | Covalent checks for source changes periodically while the link is enabled. |

Timing, pause, and Android conditions belong to the whole link. Change them on
any authorized member; the source confirms and distributes the change. An
offline source leaves requests pending. Review a conflicting request before
trying a new change. If a request's response is lost, **Try Again** reuses that
request instead of starting another transfer.

Android's **Wi-Fi only** and **While charging** choices apply to Android devices
in the link. Local Wi-Fi can work without Internet access. Each Android device
must report its current conditions; missing or stale observations pause its
transfers. The Android service and operating system must permit background work,
so a schedule is not a promise of exact wall-clock delivery.

A successful destination can finish while another is offline. An unfinished run
becomes incomplete after 24 hours. Restarting the source interrupts its active
run; review the status and use **Run Now** again. Existing files stay in place.

## Choose deletion behavior

The two controls are independent and apply to the whole link.

- **Delete destination copies when source files are deleted** is off by default.
  Keep it off to retain destination copies after a source deletion. Turn it on
  only when source deletion should also delete copies made by this link.
- **Restore files deleted at the destination** is off by default. Keep it off
  to leave a destination deletion in place, even after source edits or restarts.
  Turn it on only to download the file from the source again during the next
  run, if the source still has it.

Pause, resume, retry, or remove a link from its link controls. Removing a link
stops transfers and keeps existing files.

## Current limits

Fan-out destinations, separate family collection folders, destination-deletion
retention and explicit restoration, and shared link settings have verified
three-node journeys. Manual transfers, restart, and an actual 15-minute
scheduled transfer pass. Real Android charging and Wi-Fi conditions also block
and resume transfers. Mac HIG and VoiceOver acceptance, final package acceptance,
and laptop–Atmos validation remain open. Do not treat a local build as a
release-ready package.

See [product requirements](product/requirements.md) and the
[completion ledger](release/completion-progress.md) for current scope and
evidence. For the superseded backup-oriented setup, see the
[legacy backup setup guide](getting-started.md).

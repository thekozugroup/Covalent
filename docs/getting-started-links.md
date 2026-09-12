# Create your first one-way link

Covalent is being built as a simple wrapper for sending files one way: pair a
device, choose a source folder, then add one or more destinations. A source
never receives destination changes.

## Before you start

Use a current checkout. There is no published current release: `v0.1.0` is
historical, and `v0.2.0` has not been published. The development build paths are a
locally built Docker server, the personal-use Mac build, and the personal-use
Android debug APK.

- For an always-on destination, build Docker from this checkout. The arm64
  Docker path passed an Atmos transfer test.
- Use the Mac app on Apple Silicon for the current source/package path. Native
  macOS HIG validation is still open.
- Android's native pairing, permission-loss recovery, one-way transfer, and
  deletion-settings journey passes on an API 37 emulator. Direct folder sync is available only in the
  personal debug build and requires Android's all-files permission.

Install guidance: [macOS](platform/macos.md),
[Android](platform/android.md), and
[Docker from this checkout](../packaging/docker/README.md#personal-use-from-this-checkout).

## Create the link

1. Pair the devices and check that their confirmation codes match.
2. On the source device, open **Links**. Android calls the page **Shared folders**
   and its creation action **Create link**. Choose the source folder and a paired
   destination. Review the deletion choices below before confirming the link.
3. On the receiving device, review the incoming link, choose its destination
   folder, and accept. Current builds begin continuous transfers after both
   devices confirm the link and the source confirms its settings.
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
three-node journeys. Manual and scheduled transfer choices are still being
implemented. Android's API 37 device journey passes; whole-app design and
additional platform journeys remain open. Do not treat a local build as a release-ready package.

See [product requirements](product/requirements.md) and the
[completion ledger](release/completion-progress.md) for current scope and
evidence. For the superseded backup-oriented setup, see the
[legacy backup setup guide](getting-started.md).

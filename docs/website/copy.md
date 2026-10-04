# Covalent website copy

Editorial pack reviewed 2026-10-03. `content.json` contains the same publishable copy in structured form. See `claims-and-sources.md` for evidence and publication checks.

## Hero

Self-hosted file sharing, powered by rclone

# Share a folder. Choose where it goes.

Covalent pairs your devices and copies files from one source to one or more recipients. Set the timing once, review deletion choices, and see each recipient’s status.

Native apps for Apple Silicon Mac and Android. A browser console for Docker and Unraid.

Primary CTA: [Get Covalent](https://github.com/thekozugroup/Covalent/releases/latest)
Secondary CTA: [Read the setup guide](https://github.com/thekozugroup/Covalent/blob/1b7a38cbd05e3fb1d585fc10c648b9bc2a4f9c7d/docs/getting-started-links.md)

Personal-use packages: the Mac app is ad-hoc signed and not notarized; the Android APK is debug-signed. Read the installation guide for your device.

## Short blurb

Covalent is a self-hosted rclone wrapper for people who want a folder copied to their own devices without managing a broad set of sync controls. Choose one source and one or more recipients, share timing and deletion settings, and keep ordinary files at each destination.

## How it works

### One source, one set of choices.

Each share has a source folder, paired recipients, and settings that apply to everyone in that share.

1. **Pair your devices.** Compare the confirmation code on both devices and confirm the connection.
2. **Choose the folders.** Select the source folder. Each recipient accepts the share and chooses where its copy belongs.
3. **Set timing and deletion choices.** Run a transfer yourself, choose a schedule, or let Continuous mode check for changes. Review both deletion options before starting.
4. **See each recipient.** Check status and progress for each recipient. An offline recipient does not prevent transfers to the others.

## Features

### Send one folder to several devices

Add paired recipients to an existing share. Changes travel from the source to recipients; recipient changes do not flow back to the source or across to other recipients.

### Set timing once per share

Manual, Scheduled, and Continuous timing apply to every recipient. Settings awaiting confirmation stay visible.

### Choose what deletion means

Source deletions keep recipient copies by default. Files deleted at a recipient stay deleted there by default. Propagation and restoration are separate choices.

### Keep ordinary files

Recipient folders contain files you can open with your usual tools. Removing a share stops transfers and preserves the files already there.

### Collect from several sources

Give each contributor a separate child folder within a collection. Their files stay separate, and contributors do not receive the collection’s other contents.

### Manage shares where you use them

Use the native Mac app and menu bar, the native Android app, or the responsive server console. The console manages folders mounted into your server.

## Safeguards and limits

### Clear choices before files change.

- Pairing requires the same confirmation code on both devices.
- Devices receive access to the folders you authorize.
- Both deletion options are off by default.
- Lost folder access or an incomplete scan is not treated as a source deletion.

Covalent provides one-way file synchronization. It does not provide content deduplication or historical backup versions. A mirrored deletion is not a recoverable backup. Continuous mode checks for changes periodically and follows each platform’s background limits.

## Founder case study

Why I built Covalent

### I wanted to move files. I wanted fewer decisions.

Covalent began with a specific frustration: I wanted fewer controls and clearer choices when copying a folder to my devices.

I wanted a straightforward way to copy a folder from one of my devices to the others. Choose the source, choose the recipients, decide when to transfer, and see whether each copy is ready. That was the job I needed the software to do.

Tools such as Syncthing and Resilio Sync felt more granular than I wanted for that job. Both support one-way workflows, and their controls can be useful when you need them. For my own use, I did not want to spend time choosing conflict or version policies, or sorting accumulating copies. I wanted fewer decisions between choosing a folder and getting it onto another device.

That frustration became Covalent. I chose rclone for the file transfers and built an interface around a narrower workflow: one source folder, one or more paired recipients, and settings shared across the whole share. Covalent handles pairing, folder selection, timing, and status around the transfer engine.

The direction stays explicit. Files go from the source to its recipients. Changes at a recipient do not become edits on the source or on another recipient. If I want another device to receive the same folder, I add it to the existing share instead of configuring a separate set of timing rules.

Deletion needed more care than a single sync switch. By default, deleting a source file leaves its recipient copies in place. Deleting a file at one recipient leaves it deleted there. Propagating source deletions and restoring recipient deletions are separate choices, explained before they are enabled. Stopping a share preserves the files already on each device.

I also wanted the results to be easy to use outside the app. Recipient folders hold ordinary files. A family collection can use a separate child folder for each contributor, without sending the rest of the collection back to every source.

The tradeoff is deliberate. Covalent offers less control than a general-purpose sync tool. Its narrower model avoids two-way conflict workflows, but it is not a duplicate cleaner: renamed files can coexist, and retaining copies is a choice. It does not deduplicate content or keep historical backup versions. Continuous mode checks periodically, and Android follows the operating system’s background limits.

I wanted a file-transfer workflow I could understand from the interface. Covalent makes the source, recipients, shared settings, and deletion behavior visible together. The aim is practical: spend less time configuring the transfer, and know what the software will do.

Comparison references: [Syncthing folder types](https://docs.syncthing.net/users/foldertypes.html); [Resilio Sync one-way synchronization](https://help.resilio.com/hc/en-us/articles/204754279-Is-one-way-synchronization-possible).

## Closing CTA

### Start with one folder.

Pair two devices, choose a source and a recipient, then review the shared settings before your first transfer.

Primary: [Read the setup guide](https://github.com/thekozugroup/Covalent/blob/1b7a38cbd05e3fb1d585fc10c648b9bc2a4f9c7d/docs/getting-started-links.md)
Secondary: [View source on GitHub](https://github.com/thekozugroup/Covalent)

No hosted account or subscription required. Use separate backup software when you need recoverable history.

## SEO and social

SEO title: Covalent | Simple one-way folder sharing with rclone

Meta description: Copy a folder to your own devices with Covalent. One-way rclone transfers, shared settings, native Mac and Android apps, and a Docker web console.

Social title: Covalent: share a folder, choose where it goes

Social description: A self-hosted rclone wrapper with one source, shared settings, explicit deletion choices, and ordinary files at each destination.

Suggested social post: I built Covalent because I wanted fewer decisions when copying files between my devices. It wraps rclone in a focused one-way workflow: one source, several recipients, shared settings, and clear deletion choices.

Social image: `brand/social-card.png`.

Social image alt: Covalent logo with the message: Share a folder. Choose where it goes.

Canonical URL: unset until the public website address is available. Resolve the social image URL against that address.

## Screenshot copy

Screenshots show the shipped server console with synthetic example data. Studio, Home NAS, Archive NAS, Laptop, and Music are illustrative names.

### desktop-overview

File: `screenshots/01-share-overview.jpg`

Alt: Covalent Music share from Studio, with Home NAS and Archive NAS waiting for the next scheduled run.

Caption: See the source and each recipient in one share.

Check: Inspected capture of the shipped server console using synthetic example data.

### share-settings

File: `screenshots/02-share-settings.jpg`

Alt: Covalent Music share settings with a 60-minute schedule and both deletion options unchecked.

Caption: One set of timing and deletion choices applies to the share.

Check: Inspected capture. Both deletion choices are disabled in this example.

### add-recipient

File: `screenshots/03-add-recipient.jpg`

Alt: Covalent inline Add recipient form with Laptop selected for the Music share.

Caption: Add a paired recipient to an existing share.

Check: Inspected capture. This is an inline form, not a dialog.

### receiver-view

File: `screenshots/04-receiver-view.jpg`

Alt: Covalent recipient view of the Music share showing Studio as source and Home NAS and Archive NAS as recipients.

Caption: Recipients can see the source and everyone receiving the share.

Check: Inspected capture of the shipped server console using synthetic example data. Studio is the source; Home NAS and Archive NAS are recipients.

### mobile-web

File: `screenshots/05-mobile-web.jpg`

Alt: Covalent server console in a mobile browser showing the Music share from Studio to Home NAS and Archive NAS.

Caption: Check your server’s shares from a mobile browser.

Check: Inspected responsive server-console capture using synthetic example data. This is not a native Android or iOS application.

## Footer

One-way folder sharing. Self-hosted. Powered by rclone.

Apple Silicon Mac, Android, Docker, and Unraid. Intel Mac, iOS, and Windows clients are unsupported.

[Open source on GitHub](https://github.com/thekozugroup/Covalent)


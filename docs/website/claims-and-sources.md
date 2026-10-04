# Website claims and sources

Reviewed 2026-10-03 against this checkout and primary upstream documentation. This file is editorial guidance, not visitor-facing copy.

## Product positioning

Covalent is a native, self-hosted wrapper around rclone for one-way folder transfers. The current product has one source and one or more recipients per share. Recipient files remain ordinary files. This supersedes the older distributed encrypted-backup and bidirectional-sync positioning.

Sources: [README](../../README.md), [Product](../../PRODUCT.md), [product requirements](../product/requirements.md), [rclone engine decision](../adr/0008-rclone-one-way-links.md).

Use “copy” or “transfer” when describing file behavior. “Move files” in the founder’s motivation means getting files onto another device; it must not imply the source file is automatically removed after a successful copy.

## Claim ledger

| Website claim | Evidence | Qualification |
| --- | --- | --- |
| One source, several recipients | [Product](../../PRODUCT.md); [link guide](../getting-started-links.md) | Add paired recipients to an existing share; each receiver accepts and chooses its folder. |
| Manual, Scheduled, and Continuous timing | [synchronization](../product/synchronization.md); [requirements](../product/requirements.md) | Continuous checks periodically. Do not call it instant or promise unrestricted Android background operation. |
| Timing and deletion settings apply across a share | [Product](../../PRODUCT.md); [synchronization](../product/synchronization.md) | The source confirms and distributes revisions; pending and stale settings need visible handling. |
| Changes do not flow back from recipients | [requirements](../product/requirements.md); [public v0.2.1 release](https://github.com/thekozugroup/Covalent/releases/tag/v0.2.1) | One-way describes file content. Settings can be submitted by authorized members. |
| Source deletion keeps recipient copies by default | [synchronization](../product/synchronization.md) | Enabling propagation removes that share’s destination copies. This is not historical backup retention. |
| Recipient deletion stays deleted by default | [synchronization](../product/synchronization.md) | Restoration is a separate choice and requires the file to remain available at the source. Later source edits do not silently bypass keep-deleted behavior. |
| Removing a share preserves files | [requirements](../product/requirements.md); [public v0.2.1 release](https://github.com/thekozugroup/Covalent/releases/tag/v0.2.1) | It stops transfers; it does not remove the files already copied. |
| Several sources can contribute to a collection | [link guide](../getting-started-links.md); [synchronization](../product/synchronization.md) | Each link uses a distinct child folder. Identical names remain separate; this is not content deduplication. |
| An offline recipient does not stop other recipients | [link guide](../getting-started-links.md); [requirements](../product/requirements.md) | Do not infer an offline transfer can complete or promise a delivery deadline. |
| Pairing compares a code on both devices | [Product](../../PRODUCT.md); [link guide](../getting-started-links.md) | Discovery alone establishes no trust. |
| Folder access is authorized and checked | [engine decision](../adr/0008-rclone-one-way-links.md); [synchronization](../product/synchronization.md) | Avoid “cannot lose files” or other absolute safety claims. |
| Lost access or incomplete scans are not treated as source deletions | [synchronization](../product/synchronization.md); [requirements](../product/requirements.md) | State the behavior; do not turn it into a guarantee against all failures. |
| Native Mac and Android apps; Docker/Unraid browser console | [README](../../README.md); [public v0.2.1 release](https://github.com/thekozugroup/Covalent/releases/tag/v0.2.1) | Apple Silicon only. Personal-use installation restrictions are stated below. |
| Mobile browser console | [completion ledger](../release/completion-progress.md) | The ledger records 390-pixel browser acceptance. This does not imply a native iOS app or full platform accessibility certification. |
| No hosted account or subscription required | [product requirements](../product/requirements.md) | Users still supply devices, storage, and reachable network connectivity. |
| Open source | [public repository](https://github.com/thekozugroup/Covalent); [README license](../../README.md#license) | Do not label it permissively licensed without checking the license terms. |

## Founder narrative

The founder’s reported experience is the source for frustration with the granularity of Syncthing and Resilio Sync. It is a personal preference, not an independent usability benchmark. The copy says “felt more granular than I wanted” and “for my own use.” It does not assert that the founder personally tested every named tool. It does not claim those products are broken, bloated, slow, or unable to perform one-way transfers.

The founder’s frustration with choosing conflict/version policies and sorting accumulating copies is presented as a desire for clearer choices. Covalent avoids two-way conflict workflows but is not a duplicate cleaner: renamed files can coexist and default retention can preserve old copies.

The narrower workflow is an editorial conclusion from Covalent’s product scope. It is not a measured comparison of configuration time, number of clicks, memory use, transfer speed, or reliability.

Primary competitor sources checked on 2026-10-03:

- [Syncthing folder types](https://docs.syncthing.net/users/foldertypes.html): documents Send & Receive, Send Only, and Receive Only behavior. Syncthing supports directional workflows.
- [Syncthing configuration](https://docs.syncthing.net/users/config.html): documents folder, device, connection, versioning, scan, and advanced performance controls. This establishes configuration breadth, not that every user must configure every option.
- [Resilio Sync one-way synchronization](https://help.resilio.com/hc/en-us/articles/204754279-Is-one-way-synchronization-possible): explicitly supports one-way synchronization using read-only permissions, with distinctions for linked devices and folder types.
- [Resilio Sync folder preferences](https://help.resilio.com/hc/en-us/articles/205458125-Folder-Preferences): documents folder-specific archive, overwrite, discovery, connection, and download-priority options. This supports the description of available controls without a negative performance claim.
- [rclone copy](https://rclone.org/commands/rclone_copy/): describes ordinary copying and destination preservation. Covalent adds its own durable recipient-deletion behavior; do not attribute that behavior to unmodified rclone copy.

The first-person text is a founder narrative drafted from the supplied brief, not a recorded interview or testimonial. Do not present it as an independently collected quotation.

## Release and installation status

The public release list identifies **v0.2.1** as the latest release when checked on 2026-10-03. This checkout also contains v0.2.2 and v0.2.3 release-note drafts; their presence is not publication evidence. The copy uses a release-list CTA rather than advertising an unpublished version.

Source: [public releases](https://github.com/thekozugroup/Covalent/releases), [v0.2.1 release](https://github.com/thekozugroup/Covalent/releases/tag/v0.2.1), [README installation status](../../README.md#quick-start), [publishing runbook](../release/publishing.md).

- **macOS:** Apple Silicon personal-use package, ad-hoc signed and not notarized. Do not claim Developer ID signing, notarization, App Store distribution, or Intel Mac support.
- **Android:** debug-signed personal-use APK. Production signing and store publication are deferred. Do not call it a Play Store release or production-signed package.
- **Unraid:** Docker and an importable template. Community Applications listing is not included in the public release.
- **Docker:** released amd64 and arm64 images; use the setup guide’s verified image reference.
- **Unsupported native clients:** Intel Mac, iOS, and Windows.
- **Validation:** the ledger records an owner-accepted personal-use release with native Mac, hosted Android emulator, Docker, and real server-transfer evidence. Android physical-device and exact-final-APK execution are not established by that evidence. Earlier failures remain disclosed in the ledger. Avoid “fully tested on every device” claims.

These facts belong near installation calls to action. A signed source commit or container image does not make the Mac app Developer ID signed or the APK production signed.

## Claims excluded from the copy

- No “zero CPU,” “zero overhead,” “faster,” “smaller,” or battery-life claim without applicable measurements.
- No universal “no duplicates” claim. Distinct contributor subfolders prevent filename collisions in that structure; identical content can still exist more than once.
- No content deduplication, historical version browsing, two-way collaboration, cloud services, or distributed backup-provider claims.
- No guarantee of recoverability after source loss, ransomware, user error, disk loss, or mirrored deletion.
- No guarantee that ordinary file copying creates consistent live-database or live-appdata backups. Use an application-consistent snapshot or stop the application when required.
- No “files never leave your network” claim: users may connect devices through a reachable LAN or Tailnet, and network configuration determines the route.
- No automatic deletion of the source after copying. Covalent’s source-deletion settings control later recipient behavior.
- No third-party logos, rankings, or customer testimonials have been invented.

## Screenshot and publication checks

The five screenshot entries in `content.json` point to captures of the shipped WebUI under `screenshots/`. The scenes use synthetic example data: Studio, Home NAS, Archive NAS, Laptop, and Music. This disclosure belongs with the screenshot section. All five captures were inspected for this editorial pass. The Add recipient control is an inline form with Laptop selected. The receiver view shows Studio as source, with Home NAS and Archive NAS as recipients. These are server-console screenshots, not screenshots of the native apps.

The social image is `brand/social-card.png`. Its public URL depends on the final website address.

The setup CTA uses [a verified commit permalink](https://github.com/thekozugroup/Covalent/blob/1b7a38cbd05e3fb1d585fc10c648b9bc2a4f9c7d/docs/getting-started-links.md). The corresponding `/blob/main/` URL returned 404 on 2026-10-03. The permalink loads the one-way link guide, but its historical completion notes predate later accepted release evidence; installation status remains grounded in the current release and platform guides.

1. Keep all five inspected screenshot files with their assigned names. Recheck alt text if a capture changes.
2. Adjust alt text to the actual screen. Keep captions short and alt text descriptive; do not repeat promotional claims in alt text.
3. Confirm that any “complete” or “ready” status shown is supported by actual transfer evidence. UI status alone does not establish file identity.
4. Do not describe the mobile-web capture as a native Android or iOS application.
5. Remove tokens, private addresses, personal filenames, and device identifiers from public captures.
6. Set the final canonical URL and resolve `brand/social-card.png` to a public social-image URL before publication. No public website hostname has been supplied.
7. Preserve the personal-use signing notice near downloads and link to platform installation guidance.
8. Recheck the public release and feature claims if the site publishes after a material product change.

## Editorial consistency

`copy.md` is the reviewable prose pack. `content.json` contains equivalent copy for a website build. Hero, feature cards, founder narrative, CTA, metadata, and screenshot suggestions should stay consistent across both. The founder case study is below 600 words.

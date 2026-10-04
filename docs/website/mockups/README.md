# Covalent product preview pack

Five finished promotional compositions inspired by app-store previews: one desktop hero and four feature portraits. These are website and social assets, not store submission files or evidence of store availability.

Open `index.html` to view the set locally. The layouts work offline with the included image assets. PNG files are the masters; WebP files are smaller website versions. Use the supplied dimensions and alt text in `manifest.json`.

| Asset | Dimensions | Use |
| --- | --- | --- |
| `exports/01-desktop-and-phone` | 1920 × 1200 | Project hero; desktop-only composition (existing filename retained) |
| `exports/02-native-android` | 1080 × 1920 | Portrait feature: native Android folder setup on Pixel 6 |
| `exports/03-add-a-recipient` | 1080 × 1920 | Portrait feature: extend an existing share |
| `exports/04-shared-settings` | 1080 × 1920 | Portrait feature: timing and deletion choices |
| `exports/05-transfer-status` | 1080 × 1920 | Portrait feature: per-recipient run results |

Each asset has `.png` and `.webp` versions. `exports/contact-sheet.jpg` provides a quick overview of the complete set.

## What is real and what is illustrative

The desktop interface pixels are captures of the actual Covalent web console. Device names, share data, and run states are synthetic examples. The active scene shows a run in progress with Home NAS complete and Archive NAS waiting; it did not transfer real files and does not establish performance or reliability. No private server addresses, credentials, or personal filenames are included.

The phone now shows the **native Android app**, captured on a temporary Pixel 6 emulator running Android 16. It uses the released v0.2.1 Android package, build `7157b555a2c9f684`, and shows its real Shared folders setup screen. The capture is the full, unaltered 1080 × 2400 screenshot, scaled proportionally into Google's Pixel 6 artwork at its documented screen offset. It is not a browser recreation or an AI-generated interface. The native capture demonstrates setup, not an active phone transfer.

Google's frame is intended for websites and promotional material. These framed graphics are not store submission files; Google recommends unframed screenshots for Google Play listings. The desktop browser frame remains a presentation graphic. Keep the disclosure visible, or repeat it in an adjacent caption if you crop it out. See [asset provenance](assets/README.md) for sources and background-generation details.

The standalone interface images from 2026-10-03 are in `../screenshots/`. The two new active-state captures in `captures/` were taken on 2026-10-04 from the unchanged web interface. The scene is reproducible with `python3 docs/website/preview.py --running` from a checkout; expand **Transfer in progress** for the detailed capture. The preview is read-only and uses no production services.

## Editable source

- `index.html`: exact headlines, captions, image references, and layout templates.
- `previews.css`: dimensions, spacing, Tailwind Slate/Zinc colors, device frames, and screenshot crops.
- `../brand/covalent-mark.svg` and `covalent-mark-light.svg`: existing vector logo.
- `captures/` and `../screenshots/`: original screenshot inputs.
- `assets/`: Google's Pixel 6 frame layers, the generated abstract motion-blur background, and provenance.

The artwork is rendered HTML/CSS, keeping type and placement editable. Only the abstract blue/green/yellow background is generated imagery; interface pixels and Pixel artwork are not generated. Georgia and the system sans-serif follow the project's existing identity; font files are not redistributed. The exports were rendered on macOS with Georgia available. A different system font can change wrapping when recreating them.

Open `index.html?export=hero` at 1920 × 1200 to export the hero. The other route values are `destinations`, `recipient`, `settings`, and `status`; export these at 1080 × 1920. Capture the page without browser chrome. The gallery is responsive; the export routes deliberately retain their fixed artboard sizes. Do not stretch the images into another ratio; adapt the editable layout instead.

For web use, prefer WebP, set `width` and `height`, and keep `height: auto`. The hero can load eagerly; lazy-load supporting portraits. Important product explanations should also appear as live text beside the images.

Original Covalent artwork retains its MIT license. Google's Pixel artwork retains its separate source terms and attribution. See [ASSET-NOTICES.md](../ASSET-NOTICES.md).

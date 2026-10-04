# Covalent website handoff

Use this pack to add Covalent to the existing website as an open-source project.
It contains finished editorial copy, current product screenshots, brand assets,
and the complete repository source snapshot. Do not build a new sync service
or change the running installations as part of the website work.

## Start here

1. Read [copy.md](copy.md). Use the short blurb for the project list and the
   founder story for the project detail page.
2. Use [content.json](content.json) for structured copy, CTAs, metadata,
   screenshot paths, captions, and alt text. The prose and JSON are equivalent.
3. Open [index.html](index.html) locally to inspect the asset gallery. No
   installation, account, network request, or build step is needed.
4. Copy the `screenshots/` and `brand/` assets into the website's public assets
   directory. Update their paths in the website implementation.
5. Keep the personal-use installation note beside downloads. Review
   [claims-and-sources.md](claims-and-sources.md) when editing product claims.
6. Use [the product preview set](mockups/index.html) for a framed hero and
   portrait feature graphics. It includes PNG/WebP exports, editable layouts,
   dimensions, alt text, and [usage guidance](mockups/README.md).

## Suggested page order

Project title and short description; overview screenshot; how it works;
three focused features (one-to-many, shared settings, deletion choices);
founder story; clear limitations; setup and source links.

Use the current site's layout and navigation. Covalent's assets use serif
headings, system sans-serif body text, muted Tailwind Slate accents, light Zinc
surfaces, and compact pill controls. See [brand/USAGE.md](brand/USAGE.md).

## Screenshot facts

These are browser captures of the unmodified Covalent server console at source
revision `1b7a38cbd05e3fb1d585fc10c648b9bc2a4f9c7d`, captured on 2026-10-03.
The local, read-only preview supplies synthetic data. No production configuration
was changed and no transfer ran for the screenshots.

Display this short disclosure under the gallery: **“Current server interface
with example devices and data.”** Keep the source/recipient and deletion
explanations visible when cropping. Images are already small JPEG files;
avoid enlarging them beyond their native resolution.

| Asset | Intended use | What it shows |
| --- | --- | --- |
| `screenshots/01-share-overview.jpg` | Lead product image | Music from Studio to Home NAS and Archive NAS, hourly. |
| `screenshots/02-share-settings.jpg` | Feature detail | Shared timing and both deletion options off. |
| `screenshots/03-add-recipient.jpg` | Expansion flow | Laptop selected in the inline Add recipient form; not submitted. |
| `screenshots/04-receiver-view.jpg` | Cross-device clarity | Home NAS sees the same share, source, and two recipients. |
| `screenshots/05-mobile-web.jpg` | Responsive view | The same server console in a 390-pixel-wide browser. |

The narrow capture is **mobile web**, not the native Android app or an iOS app.
All names are examples. The screenshots are presentation evidence, not proof
that the simulated transfer completed. Current release verification is documented
separately in the repository.

The SVG logo geometry comes from the console's existing overlapping-circle mark.
The social card is a promotional graphic, not a screenshot. Use
`brand/social-card.png` for Open Graph/Twitter previews (1200 × 630); keep the SVG
as editable source. Original screenshot files and SVG sources are included.

## Copy and behavior to preserve

- The founder story expresses a preference for fewer choices. It does not
  present a benchmark or claim competing tools are defective.
- One-way means file contents go from source to recipients. Shared settings
  can be proposed from authorized members; the source confirms revisions.
- Copying does not delete the source automatically. Deletion propagation and
  restoring files deleted at a recipient are independent, default-off choices.
- Plain files are usable outside Covalent. Covalent is not a duplicate cleaner,
  versioned recovery system, or a live-database backup guarantee.
- The current public native download is v0.2.1. Mac is ad-hoc signed and not
  notarized; Android is debug-signed. Later draft releases are not public downloads.
- The screenshot source is newer than that native download. Do not imply every
  pictured web feature is available in every older native package.

## Publication steps for the website agent

Set `seo.canonicalUrl` to the final project-page URL. Convert the relative social
image path to an absolute public URL. Keep alt text and explicit image dimensions;
use responsive images with `height: auto`. Give the lead screenshot priority and
lazy-load the rest. Keep all product copy as live text, not text embedded in images.

The setup CTA uses a verified source permalink because the main-branch guide
was missing at capture time. Once the relevant changes reach the repository's
default branch, replace that permalink with a verified default-branch link.
Do not claim a PR is merged or a new release published merely because this pack
contains source changes.

Before publishing, check links, small-screen layout, keyboard focus, image loading,
metadata URLs, and the current release status. Preserve MIT and third-party
notices from [ASSET-NOTICES.md](ASSET-NOTICES.md) when redistributing assets.

## Repository and reproduction

The ZIP's `repository/` directory is a clean source snapshot, with no `.git`,
credentials, runtime state, temporary builds, or deployment artifacts. The outer
`MANIFEST.json` identifies the source commit and every file; `SHA256SUMS` verifies
their integrity. It is source code, not a prebuilt installer.

The refreshed README and setup, contribution, product, and security documentation
are included. [repo-review.md](repo-review.md) describes their scope and the
distribution limitations that remain.

To inspect the screenshot scenes from a checkout:

```sh
python3 docs/website/preview.py
python3 docs/website/preview.py --receiver
```

Open the printed localhost URLs. These are read-only example servers with no
real pairing, file transfers, or setting writes. Stop each with Ctrl-C. For
repackaging a clean checkout, run `python3 docs/website/package.py`.

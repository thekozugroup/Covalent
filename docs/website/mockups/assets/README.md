# Preview asset provenance

## Android screenshot and Pixel frame

`../captures/android-pixel-6.png` is an unaltered 1080 × 2400 native Android
screenshot captured with MobileCLI on 2026-10-04. The temporary Pixel 6 emulator
ran Android 16 and Covalent v0.2.1, build `7157b555a2c9f684`. The screen shows
Shared folders setup after enabling the phone's folder service. No real device
was paired, no production data was accessed, and no phone transfer is implied.
The emulator and temporary capture tools were removed after export.

APK SHA-256: `7157b555a2c9f684aa6c8f06a0ec94079cfe614f633065b4189c423e5cbadf6b`.

Google's [Device Art Generator](https://developer.android.com/distribute/marketing-tools/device-art-generator)
supplies the Pixel 6 artwork:

- [Background layer](https://developer.android.com/static/distribute/marketing-tools/device-art-resources/pixel_6/port_back.png)
- [Foreground layer](https://developer.android.com/static/distribute/marketing-tools/device-art-resources/pixel_6/port_fore.png)
- [Placement manifest](https://developer.android.com/static/distribute/marketing-tools/device-art-resources/device-art-generator.js)

Both layers are 1216 × 2562. The screen is 1080 × 2400, at x=62 and y=69.
The HTML/CSS layers the screenshot between these two original PNGs. All three
layers receive the same scale; no stretching, synthesized status bars, or
invented camera cutouts are applied. The Pixel camera and rounded screen edges
come from Google's original foreground layer.

Google describes the generator for websites and promotional material. Use
unframed screenshots for Google Play submissions, following its guidance.
See [asset notices](../../ASSET-NOTICES.md) for attribution and source terms.

## Abstract background

`motion-meadow.png` is the original generated background. `motion-meadow.webp`
is its compressed website derivative. Generated on 2026-10-04 using the built-in
image-generation tool; no CLI fallback or external stock photo was used.

Final prompt:

> Generate only an abstract photographic background for Covalent software promotional artwork. Landscape 1920x1200 composition. A sunlit meadow and blue sky transformed by very strong horizontal camera panning motion blur: elongated silky streaks of fresh lime and sage green, pale butter yellow, soft blue, teal, with a little lavender. No identifiable objects, no horizon line, no text, no logo, no device, no UI. Richer color near the outer sides and lower corners; the center and upper middle luminous pale cream and light blue so dark marketing headlines and app screenshots can be overlaid clearly. The effect should feel like an intentionally motion-blurred landscape photograph, not a flat gradient, with fine soft horizontal variations and a calm airy mood. Edge-to-edge full bleed. High quality natural optical blur. This is a background layer only.

The tool returned 1586 × 992 pixels. The editable layout uses cover sizing and
a translucent surface behind headings for contrast. Only the background is
generated; the interface and device frame remain sourced pixels.

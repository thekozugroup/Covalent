# Android open source notices

Release builds generate the bundled notice set from the exact Android arm64 and x86_64
Syncthing dependency graphs produced with CGO enabled, the pinned Syncthing source export,
the private build module cache, the pinned Go distribution, the reviewed guardian source,
and Covalent's preserved Open Font License text. The generator records exact source hashes
and emits both a combined readable notice and a byte-exact manifest. The app verifies their
declared lengths and SHA-256 digests before showing the combined notice under Settings.

The Android build also retains bounded final-link maps, exact Clang driver traces, and complete
dynamic-dependency reports for the worker, guardian, and JNI library. It classifies every NDK
input that contributed sections, records the exact pinned NDK revision, and bundles that NDK's
full `NOTICE` and `NOTICE.toolchain` files with verified lengths and SHA-256 digests. Android's
dynamic platform libraries remain supplied by the device rather than copied into this APK.

The generated manifest deliberately reports `texts-collected-review-required`. These records
are an inventory and distribution mechanism, not legal approval. Release review must still
confirm that the classified link inputs and copied notices satisfy each component's terms.

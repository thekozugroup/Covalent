# Android open source notices

Release builds generate the bundled notice set from the exact Android arm64 and x86_64
Syncthing dependency graphs produced with CGO enabled, the pinned Syncthing source export,
the private build module cache, the pinned Go distribution, the reviewed guardian source,
and Covalent's preserved Open Font License text. The generator records exact source hashes
and emits both a combined readable notice and a byte-exact manifest. The app verifies their
declared lengths and SHA-256 digests before showing the combined notice under Settings.

The generated manifest deliberately reports `texts-collected-review-required`. It is an
inventory and distribution mechanism, not legal approval. Before a release, the Android NDK
and Android platform runtime linkage must still be classified against the exact packaged ELF
dependency reports. That review is separate because bionic and platform libraries are supplied
by Android rather than copied into this APK.

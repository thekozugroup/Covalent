# Reproducible Android sync workers

The restricted rclone worker used an absolute temporary external-linker path in
Go linker flags. Go includes those flags in the executable's build identity, so
otherwise identical source/toolchain builds produced different worker bytes.

The builder now names the same ABI-specific guarded wrapper by its stable
basename and resolves it through its exclusive private build directory in PATH.
The real pinned NDK compiler, single-final-link marker, provenance classification,
dependency/license collection, runtime budgets and16KiB alignment checks remain.
Build IDs are retained. No file is patched after linking.

Two complete producer runs in different directories, including one containing
spaces, produced identical rclone and guardian objects for ARM64 and x86_64.
The comparison refuses the earlier varying worker. The Rust Pi daemon separately
reproduced its prior same-source hash. These are component-level results on this
host, not a complete APK/image, cross-host or physical-device qualification.

Android foundation CI now compares its normal producer output with a second
fresh full producer run. `check-android-worker-reproducibility.py` compares the
source/toolchain receipts and all four bounded regular binary files. The normal
producer remains responsible for ELF, provenance and redistribution validation;
the CI step supplies independent fresh executions. A comparison alone cannot
prove how two caller-supplied directories were produced.

Bounded source/input/output evidence is in
`artifacts/evidence/android-worker-reproducibility/checks.json`. Current-head
hosted CI, normal package/native execution and the physical gold-image gates
remain separate requirements.

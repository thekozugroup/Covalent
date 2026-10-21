# Testing packaged workers inside App Sandbox

The packaged macOS workers have exactly the sandbox and inheritance
entitlements. The previous CI command launched the Rust integration test
directly from Cargo's ordinary shell parent. A new test-only native parent
supplies the sandbox instead. Production executables retain their archived
signatures and bytes.

[Apple's sandbox inheritance reference](https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/EntitlementKeyReference/Chapters/EnablingAppSandbox.html)
requires the child to inherit from its parent through NSTask or posix_spawn.
Its two sandbox entitlement keys belong to the child; the main parent must
not enable inheritance. The QA parent follows this model, with network
permissions and private container storage. It has no user-selected-folder or
personal Pictures-folder entitlement.

Run on Apple Silicon macOS with the just-built, hardened-runtime archive:

```sh
python3 -B scripts/test-macos-sandboxed-address-handoff.py \
  /absolute/CovalentMac.xcarchive/Products/Applications/Covalent.app \
  /absolute/new-evidence-directory
```

The runner verifies the complete app, selects the single actual Cargo test
executable from build messages, copies the archived worker and guardian, and
signs only its owned test executable and native parent. It checks the actual
parent/test entitlement dictionaries. Before testing, the parent must observe
a kernel denial when opening an existing synthetic file outside its sandbox
and prove that its private temporary directory is writable. The unchanged
worker must also run its version command successfully.

The real-transfer scenario keeps its original 240-second deadline.
It must acknowledge all three generations, both endpoint moves, retained
identities/settings, exact request replays and original source bytes. A zero-test
success cannot satisfy the required result and phase markers. Cleanup must
confirm stopped runtimes and an absent fixture directory. The runner rechecks
the archive and copied executable hashes afterward. Logs and the receipt are
retained in CI on either outcome; executables and fixture contents are excluded
from uploads. The parent passes a fixed minimal environment to children.

Completed manual-request replays follow the same explicit HTTP 503
`folder_sync_busy` retry contract as their initial submission, using the same
request bytes, generation and original 60-second per-operation deadline. The
service intentionally refuses concurrent mutation-lock access; its journal
recognizes the retained request once that lock is available. Other errors fail
immediately. Replay busy-response counts are logged. No production lock,
journal, signature or deadline is relaxed.

This checks synthetic loopback transfers using an inherited sandbox. It does
not exercise user-granted bookmarks, an installed app's UI, Android SAF,
physical roaming, Google Photos, or a Pi image. Those acceptance gates remain
separate.

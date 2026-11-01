# Automatic-update choice: core contract

Frozen host Rust1.97.1 qualification:186 core library tests pass, no ignored/filtered tests in the main run. The two explicitly scoped subprocess lock probes each filter unrelated tests as expected; they are not extra full-suite runs. Seven new tests cover the signed update-choice boundary. All core Clippy targets/features pass with warnings denied. Cargo.lock and dependencies are unchanged.

This is a pure signature/canonical-command/revision contract. No durable Pi policy store, privileged handler, transport route or companion switch is exposed. Verification does not consume a revision; actual atomic effects and lost-reply handling remain required before end-to-end or replay-protection claims. The feature branch preserves the existing PR60 API37 run.

No new VM/AVD/container/server or device operation. Temporary compiler outputs are removed after evidence preservation. Source hashes bind exact functional inputs; compressed logs retain original bytes.

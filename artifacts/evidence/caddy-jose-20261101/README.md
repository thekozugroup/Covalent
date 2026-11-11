# Caddy source security qualification — 2026-11-01

Real stripped Linux ELF files,149-package file SBOMs and fresh Grype reports bind both target builds. Eight JOSE rejection cases, upstream HTML tests,18 evidence tests and eight host TLS/restart checks pass. No excludes or scan-gate change.

The initial JOSE-only summaries retain two unscored HTML findings that prompted the additional official x/net fix. The final reports retain the unrelated OpenPGP module-level match; its affected packages are absent.

reference-only/ uses unchanged reviewed CA metadata for source reference generation. It is not an installed APK receipt or native image proof. Full native image gates remain mandatory. The test CA keys and binaries are disposable and are not retained here. No Pi/phone/account/import/journal mutation occurred.

Compressed files preserve original bytes. source-inputs.json binds functional inputs. SHA256SUMS binds the retained evidence.

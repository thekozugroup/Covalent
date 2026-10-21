# Alpine musl 1.2.5-r24 recipe

Unmodified recipe directory from Alpine aports commit
`fd05fc094264bce4fb76efc6c7f61ba4abfcb609`, main/musl. Alpine's signed packages
identify this commit. The upstream revision adds the regex overflow fix
`CVE-2026-107891.patch` and renames the already present iconv patch to its CVE name.
The patch refers to upstream musl commit `7813e9119eb5fe681b6750b8a033a0490bca0941`.

Primary source:
https://git.alpinelinux.org/aports/tree/main/musl?id=fd05fc094264bce4fb76efc6c7f61ba4abfcb609

All 24 regular recipe files are retained in `aport/`. Their Git blobs, complete
directory tree, SHA256, sizes and APKBUILD SHA512 checksums are recorded in
`../runtime-source-lock.json`. The complete recipe tree SHA1 is
`ac111a3e906e374f6296091894ba3eb26348ee36`. The musl source archive remains the
same hash-locked upstream 1.2.5 release. Runtime packages are installed from
Alpine with signature checks; these source recipes are never executed.

The primary server was ahead of the GitHub mirror when reviewed. The recipe is
vendored to avoid making builds depend on that mirror delay or primary-server
rate limits. Only an exact primary-server file URL at the locked commit is
accepted by the collector; download redirects remain refused. Existing size,
hash, full-tree, license and final installed-package checks remain mandatory.

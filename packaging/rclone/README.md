# Bundled rclone

Covalent uses upstream rclone v1.75.1, commit
`687d264b689b8c49a67e2e52a8a5e0caa01c04ce`, as its selected transfer engine.
This entry point imports only the local, SFTP, and WebDAV backends and the
commands needed by the wrapper. It does not modify upstream transfer code.

This entry point is used by the macOS, Android, and Docker packages. Rclone is
the sole active transfer engine; the completion ledger records remaining release checks.

Build with the pinned repository Go toolchain, a temporary `GOCACHE` and
`GOMODCACHE`, and an explicit output outside the checkout:

```sh
go build -mod=readonly -trimpath -buildvcs=false \
  -ldflags '-s -w -X github.com/rclone/rclone/fs.Version=v1.75.1' \
  -o /absolute/temporary/path/covalent-rclone .
```

Use a private configuration file and private cache/temp directories. A network
SFTP connection must verify the paired host key. The remote-control server is
not included. Upstream rclone is MIT licensed; packages must include its license
and the selected target's dependency notices.

The only Covalent-specific command behavior is the SFTP authorization helper.
When invoked with no arguments and `COVALENT_RCLONE_AUTH_MAP` set, it reads one
upstream authentication request from stdin and returns the exact paired link's
backend. The node supplies a private executable symlink without spaces because
upstream's auth-proxy argument parser splits on whitespace. No shell is used.
The owner-only map contains version 1 and entries with a link UUID username,
base64 SSH Ed25519 public key, and either a canonical local root or an
authenticated numeric-loopback WebDAV root. WebDAV passwords use upstream's
`obscure -` command with stdin; obscuring is reversible and grants remain private.
The node restarts receiver access on revocation because upstream caches grants.

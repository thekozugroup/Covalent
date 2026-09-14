// Covalent builds Caddy from source rather than lifting the binary out of
// caddy:2.11.4-alpine, whose vendored build is linked against go1.26.3. The
// selected source is upstream snapshot
// v2.11.5-0.20260711231708-b2693fb63a30, the CEL compatibility commit for
// cel-go v0.29.2; it is 33 commits after v2.11.4 and is pinned in go.mod.
//
// The imports below register the Caddyfile, TLS, proxy, header, and compression
// features used by packaging/docker/Caddyfile and its runtime checks. Keeping
// this explicit set avoids shipping unrelated standard-distribution modules.
// The pinned upstream snapshot and its reviewed delta are documented in
// docs/security/container-image-vulnerabilities.md.
package main

import (
	_ "github.com/caddyserver/caddy/v2/caddyconfig/caddyfile"
	caddycmd "github.com/caddyserver/caddy/v2/cmd"
	_ "github.com/caddyserver/caddy/v2/modules/caddyevents"
	_ "github.com/caddyserver/caddy/v2/modules/caddyhttp"
	_ "github.com/caddyserver/caddy/v2/modules/caddyhttp/encode/gzip"
	_ "github.com/caddyserver/caddy/v2/modules/caddyhttp/encode/zstd"
	_ "github.com/caddyserver/caddy/v2/modules/caddyhttp/headers"
	_ "github.com/caddyserver/caddy/v2/modules/caddyhttp/reverseproxy"
	_ "github.com/caddyserver/caddy/v2/modules/caddypki"
	_ "github.com/caddyserver/caddy/v2/modules/caddytls"
	_ "github.com/caddyserver/caddy/v2/modules/caddytls/standardstek"
	_ "github.com/caddyserver/caddy/v2/modules/filestorage"
	_ "github.com/caddyserver/caddy/v2/modules/logging"
)

func main() {
	caddycmd.Main()
}

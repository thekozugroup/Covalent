// Covalent builds Caddy from source rather than lifting the binary out of
// caddy:2.11.4-alpine, whose vendored build is linked against go1.26.3. The
// selected source is upstream snapshot
// v2.11.5-0.20260711231708-b2693fb63a30, the CEL compatibility commit for
// cel-go v0.29.2; it is 33 commits after v2.11.4 and is pinned in go.mod.
//
// This is the same three-line program `xcaddy` generates and the official
// Caddy image builds: it is a *consumer* module of github.com/caddyserver/caddy/v2,
// so the resulting binary is stock upstream Caddy with its standard module set
// and no third-party plugins. The pinned upstream snapshot and its reviewed
// delta are documented in docs/security/container-image-vulnerabilities.md.
// What this consumer module buys is control of the toolchain and module graph.
package main

import (
	caddycmd "github.com/caddyserver/caddy/v2/cmd"

	// Registers every module in Caddy's standard distribution, which is what
	// makes this binary equivalent to an official `caddy` build.
	_ "github.com/caddyserver/caddy/v2/modules/standard"
)

func main() {
	caddycmd.Main()
}

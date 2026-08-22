// Covalent builds Caddy from source rather than lifting the binary out of
// caddy:2.11.4-alpine, whose vendored build is linked against go1.26.3.
//
// This is the same three-line program `xcaddy` generates and the official
// Caddy image builds: it is a *consumer* module of github.com/caddyserver/caddy/v2,
// so the resulting binary is stock Caddy with the standard module set — no
// plugins added, no behaviour changed. What it buys is control of the
// toolchain and of the module graph, both of which are properties of the
// building module rather than of Caddy itself.
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

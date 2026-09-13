// Covalent bundles upstream rclone with only the backends and commands it uses.
// Transfer algorithms, SFTP, and WebDAV remain upstream implementations.
package main

import (
	"fmt"
	"os"

	_ "github.com/rclone/rclone/backend/local"
	_ "github.com/rclone/rclone/backend/sftp"
	_ "github.com/rclone/rclone/backend/webdav"
	"github.com/rclone/rclone/cmd"
	_ "github.com/rclone/rclone/cmd/copy"
	_ "github.com/rclone/rclone/cmd/copyto"
	_ "github.com/rclone/rclone/cmd/lsjson"
	_ "github.com/rclone/rclone/cmd/obscure"
	_ "github.com/rclone/rclone/cmd/serve/sftp"
	_ "github.com/rclone/rclone/cmd/sync"
	_ "github.com/rclone/rclone/cmd/version"
)

func main() {
	if path := os.Getenv("COVALENT_RCLONE_AUTH_MAP"); len(os.Args) == 1 && path != "" {
		if err := authorize(path, os.Stdin, os.Stdout); err != nil {
			fmt.Fprintln(os.Stderr, "Covalent transfer authorization failed")
			os.Exit(1)
		}
		return
	}
	cmd.Main()
}

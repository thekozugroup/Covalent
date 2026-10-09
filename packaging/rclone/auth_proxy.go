package main

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strconv"

	"golang.org/x/crypto/ssh"
	"golang.org/x/sys/unix"
)

const maxMapBytes = 1 << 20

var errDenied = errors.New("authorization denied")
var linkUsername = regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`)

type backend struct {
	Type   string `json:"type"`
	Root   string `json:"_root"`
	URL    string `json:"url,omitempty"`
	Vendor string `json:"vendor,omitempty"`
	User   string `json:"user,omitempty"`
	Pass   string `json:"pass,omitempty"` // Upstream rclone-obscured value, still a secret.
}

type capability struct {
	Username  string  `json:"username"`
	PublicKey string  `json:"publicKey"`
	Backend   backend `json:"backend"`
}

type capabilityMap struct {
	Version int          `json:"version"`
	Entries []capability `json:"entries"`
}

// This helper is invoked by rclone after SSH verifies possession of the key.
// The node alone writes the private map; no request can choose a backend root.
func authorize(path string, input io.Reader, output io.Writer) error {
	if !filepath.IsAbs(path) {
		return errDenied
	}
	parent, err := os.Lstat(filepath.Dir(path))
	if err != nil || !parent.IsDir() || parent.Mode().Perm() != 0700 {
		return errDenied
	}
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	if err != nil {
		return errDenied
	}
	file := os.NewFile(uintptr(fd), path)
	defer file.Close()
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm() != 0600 || info.Size() > maxMapBytes {
		return errDenied
	}
	var stat unix.Stat_t
	if unix.Fstat(fd, &stat) != nil || stat.Uid != uint32(os.Geteuid()) {
		return errDenied
	}
	var grants capabilityMap
	if decode(file, maxMapBytes, &grants) != nil || grants.Version != 1 || len(grants.Entries) > 1024 {
		return errDenied
	}
	var request struct {
		User      string `json:"user"`
		PublicKey string `json:"public_key"`
		Pass      string `json:"pass"`
		ClientIP  string `json:"client_ip"`
	}
	if decode(input, 16384, &request) != nil || request.Pass != "" || request.PublicKey == "" {
		return errDenied
	}
	// Validate the entire map, including duplicate usernames, before responding.
	roots := make(map[string]backend)
	var matched *backend
	for _, grant := range grants.Entries {
		keyBytes, err := base64.StdEncoding.Strict().DecodeString(grant.PublicKey)
		if err != nil || !linkUsername.MatchString(grant.Username) || !grant.Backend.valid() {
			return errDenied
		}
		key, err := ssh.ParsePublicKey(keyBytes)
		if err != nil || key.Type() != ssh.KeyAlgoED25519 {
			return errDenied
		}
		if prior, exists := roots[grant.Username]; exists && prior != grant.Backend {
			return errDenied
		}
		roots[grant.Username] = grant.Backend
		if request.User == grant.Username && request.PublicKey == grant.PublicKey {
			value := grant.Backend
			matched = &value
		}
	}
	if matched == nil {
		return errDenied
	}
	if matched.Type == "local" {
		canonical, err := filepath.EvalSymlinks(matched.Root)
		if err != nil || canonical != matched.Root {
			return errDenied
		}
		info, err := os.Stat(matched.Root)
		if err != nil || !info.IsDir() {
			return errDenied
		}
	}
	return json.NewEncoder(output).Encode(matched)
}

func decode(input io.Reader, limit int64, value any) error {
	data, err := io.ReadAll(io.LimitReader(input, limit+1))
	if err != nil || int64(len(data)) > limit {
		return errDenied
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(value); err != nil {
		return err
	}
	if decoder.Decode(new(any)) != io.EOF {
		return errDenied
	}
	return nil
}

func (b backend) valid() bool {
	switch b.Type {
	case "local":
		return filepath.IsAbs(b.Root) && filepath.Clean(b.Root) == b.Root && b.URL == "" && b.Vendor == "" && b.User == "" && b.Pass == ""
	case "webdav":
		u, err := url.Parse(b.URL)
		if err != nil || u.Scheme != "http" || u.Hostname() != "127.0.0.1" || u.Path != "/" || u.RawQuery != "" || u.Fragment != "" || u.User != nil || u.RawPath != "" || u.ForceQuery {
			return false
		}
		port, err := strconv.Atoi(u.Port())
		return err == nil && port > 0 && port <= 65535 && b.Root == "" && b.Vendor == "other" && len(b.User) > 0 && len(b.User) <= 1024 && len(b.Pass) > 0 && len(b.Pass) <= 4096
	default:
		return false
	}
}

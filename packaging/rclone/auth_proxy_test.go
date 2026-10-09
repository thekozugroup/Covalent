package main

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"golang.org/x/crypto/ssh"
)

const testUser = "00000000-0000-4000-8000-000000000001"

func testKey(t *testing.T) string {
	t.Helper()
	public, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	key, err := ssh.NewPublicKey(public)
	if err != nil {
		t.Fatal(err)
	}
	return base64.StdEncoding.EncodeToString(key.Marshal())
}

func TestAuthorizationBoundary(t *testing.T) {
	dir, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "map.json")
	key := testKey(t)
	local := backend{Type: "local", Root: dir}
	grant := capability{Username: testUser, PublicKey: key, Backend: local}
	request, _ := json.Marshal(map[string]string{"user": testUser, "public_key": key, "client_ip": "192.0.2.1"})
	cases := []struct {
		name    string
		entries []capability
		request string
		allowed bool
	}{
		{"approved root", []capability{grant}, string(request), true},
		{"wrong link", []capability{grant}, strings.Replace(string(request), testUser, "00000000-0000-4000-8000-000000000002", 1), false},
		{"wrong key", []capability{grant}, strings.Replace(string(request), key, testKey(t), 1), false},
		{"password only", []capability{grant}, `{"user":"` + testUser + `","pass":"secret"}`, false},
		{"unknown request field", []capability{grant}, strings.TrimSuffix(string(request), "}") + `,"root":"/"}`, false},
		{"conflicting link roots", []capability{grant, {Username: testUser, PublicKey: testKey(t), Backend: backend{Type: "local", Root: filepath.Dir(dir)}}}, string(request), false},
		{"another authorized peer same root", []capability{grant, {Username: testUser, PublicKey: testKey(t), Backend: local}}, string(request), true},
		{"unavailable unrelated root stays isolated", []capability{grant, {Username: "00000000-0000-4000-8000-000000000002", PublicKey: testKey(t), Backend: backend{Type: "local", Root: filepath.Join(dir, "missing")}}}, string(request), true},
		{"unavailable selected root denied", []capability{{Username: testUser, PublicKey: key, Backend: backend{Type: "local", Root: filepath.Join(dir, "missing")}}}, string(request), false},
		{"revoked grant", nil, string(request), false},
		{"trailing request", []capability{grant}, string(request) + `{}`, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			data, _ := json.Marshal(capabilityMap{Version: 1, Entries: tc.entries})
			if err := os.WriteFile(path, data, 0600); err != nil {
				t.Fatal(err)
			}
			var output bytes.Buffer
			err := authorize(path, strings.NewReader(tc.request), &output)
			if (err == nil) != tc.allowed {
				t.Fatalf("allowed=%v, error=%v", tc.allowed, err)
			}
			if !tc.allowed && output.Len() != 0 {
				t.Fatal("denied request returned capability data")
			}
			if tc.allowed {
				var got backend
				if err := json.Unmarshal(output.Bytes(), &got); err != nil || got != local {
					t.Fatalf("unexpected backend: %v", err)
				}
			}
		})
	}
	data, _ := json.Marshal(capabilityMap{Version: 1, Entries: []capability{grant}})
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	t.Run("shared permissions rejected", func(t *testing.T) {
		if err := os.Chmod(path, 0644); err != nil {
			t.Fatal(err)
		}
		defer os.Chmod(path, 0600)
		if authorize(path, bytes.NewReader(request), &bytes.Buffer{}) == nil {
			t.Fatal("shared file accepted")
		}
	})
	t.Run("symlink map rejected", func(t *testing.T) {
		link := filepath.Join(dir, "map-link")
		if err := os.Symlink(path, link); err != nil {
			t.Fatal(err)
		}
		if authorize(link, bytes.NewReader(request), &bytes.Buffer{}) == nil {
			t.Fatal("symlink accepted")
		}
	})
	t.Run("oversized map rejected", func(t *testing.T) {
		if err := os.WriteFile(path, bytes.Repeat([]byte(" "), maxMapBytes+1), 0600); err != nil {
			t.Fatal(err)
		}
		if authorize(path, bytes.NewReader(request), &bytes.Buffer{}) == nil {
			t.Fatal("oversized map accepted")
		}
	})
}

func TestWebDAVCapabilityIsLoopbackOnly(t *testing.T) {
	valid := backend{Type: "webdav", URL: "http://127.0.0.1:49152/", Vendor: "other", User: "private-user", Pass: "upstream-obscured-secret"}
	if !valid.valid() {
		t.Fatal("valid loopback capability rejected")
	}
	for _, address := range []string{"http://192.0.2.1:49152/", "http://localhost:49152/", "http://127.0.0.1:0/", "http://127.0.0.1:65536/", "http://127.0.0.1:49152/another-grant", "http://127.0.0.1:49152/?override=true", "http://user@127.0.0.1:49152/", "http://127.0.0.1:49152/#other"} {
		t.Run(address, func(t *testing.T) {
			value := valid
			value.URL = address
			if value.valid() {
				t.Fatal("unsafe capability accepted")
			}
		})
	}
}

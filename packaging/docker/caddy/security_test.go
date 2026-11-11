package main

import (
	"encoding/base64"
	"encoding/json"
	"strings"
	"testing"

	josev3 "github.com/go-jose/go-jose/v3"
	josev4 "github.com/go-jose/go-jose/v4"
)

// Both versions are linked: Caddy's internal CA still imports JOSE v3.
func TestJWKSecurityBoundaries(t *testing.T) {
	oversized := make([]byte, 513)
	oversized[0] = 1 // 4097 bits, one above the default RSA modulus limit.
	large := base64.RawURLEncoding.EncodeToString(oversized)
	private := `{"kty":"RSA","n":"EQ","e":"AQAB","d":"AQ","p":"Ag","q":"Aw"}`
	implementations := []struct {
		name   string
		key    func([]byte) error
		header func(string) error
	}{
		{"v3", func(b []byte) error { var k josev3.JSONWebKey; return json.Unmarshal(b, &k) },
			func(s string) error { _, err := josev3.ParseSigned(s); return err }},
		{"v4", func(b []byte) error { var k josev4.JSONWebKey; return json.Unmarshal(b, &k) },
			func(s string) error {
				_, err := josev4.ParseSigned(s, []josev4.SignatureAlgorithm{josev4.RS256})
				return err
			}},
	}
	for _, impl := range implementations {
		t.Run(impl.name, func(t *testing.T) {
			for _, tc := range []struct{ name, input, want string }{
				{"oversized public", `{"kty":"RSA","n":"` + large + `","e":"AQAB"}`, "modulus too large"},
				{"oversized private", `{"kty":"RSA","n":"` + large + `","e":"AQAB","d":"AQ","p":"Ag","q":"Aw"}`, "modulus too large"},
				{"mismatched factors", private, "p * q != n"},
			} {
				t.Run(tc.name, func(t *testing.T) {
					if err := impl.key([]byte(tc.input)); err == nil || !strings.Contains(err.Error(), tc.want) {
						t.Fatalf("expected %q rejection, got %v", tc.want, err)
					}
				})
			}
			t.Run("private header", func(t *testing.T) {
				header := base64.RawURLEncoding.EncodeToString([]byte(`{"alg":"RS256","jwk":` + private + `}`))
				if err := impl.header(header + ".e30.AQ"); err == nil || !strings.Contains(err.Error(), "JWK contains private parameters") {
					t.Fatalf("expected private-header rejection, got %v", err)
				}
			})
		})
	}
}

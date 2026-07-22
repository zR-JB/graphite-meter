package auth

import (
	"net/url"
	"testing"
)

func FuzzParsePasswordHash(f *testing.F) {
	f.Add("$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
	f.Add("")
	f.Fuzz(func(t *testing.T, value string) {
		_, _, _ = parsePasswordHash(value)
	})
}

func FuzzOIDCCallbackParameterCardinality(f *testing.F) {
	f.Add("state=value&code=value")
	f.Add("state=one&state=two&code=value")
	f.Fuzz(func(t *testing.T, raw string) {
		values, err := url.ParseQuery(raw)
		if err != nil {
			return
		}
		_, _ = exactlyOne(values, "state")
		_, _ = exactlyOne(values, "code")
	})
}

package auth

import (
	"net/url"
	"testing"
)

func FuzzParsePasswordHash(f *testing.F) {
	for _, seed := range []string{
		"$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
		"",
		"$argon2id$v=19$m=19456,t=2,p=1$$",          // empty salt/key fields
		"$argon2id$v=18$m=19456,t=2,p=1$AA$AA",      // wrong version
		"$argon2i$v=19$m=19456,t=2,p=1$AA$AA",       // wrong variant
		"$argon2id$v=19$m=x,t=y,p=z$AA$AA",          // non-numeric params
		"$argon2id$v=19$m=19456,t=2,p=1$!!!$AAA",    // invalid base64
		"$argon2id$v=19$m=99999999,t=99,p=99$AA$AA", // out-of-policy params
		"$$$$$$",           // all-empty fields
		"not-a-phc-string", // no structure
	} {
		f.Add(seed)
	}
	f.Fuzz(func(t *testing.T, value string) {
		_, _, _ = parsePasswordHash(value)
	})
}

func FuzzOIDCCallbackParameterCardinality(f *testing.F) {
	for _, seed := range []string{
		"state=value&code=value",
		"state=one&state=two&code=value",
		"code=value",                  // missing state
		"state=value",                 // missing code
		"state=&code=",                // empty values
		"state=a&code=b&error=denied", // provider error present
		"state=a&code=b&iss=x&iss=y",  // duplicate iss
		"%zz",                         // malformed query escaping
	} {
		f.Add(seed)
	}
	f.Fuzz(func(t *testing.T, raw string) {
		values, err := url.ParseQuery(raw)
		if err != nil {
			return
		}
		_, _ = exactlyOne(values, "state")
		_, _ = exactlyOne(values, "code")
	})
}

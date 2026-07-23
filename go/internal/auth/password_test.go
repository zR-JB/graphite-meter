package auth

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestPasswordHashParametersAndVerification(t *testing.T) {
	h, err := HashPassword("correct horse battery staple")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(h, "$argon2id$v=19$m=19456,t=2,p=1$") {
		t.Fatalf("unexpected PHC: %s", h)
	}
	if !verifyPassword(h, "correct horse battery staple") || verifyPassword(h, "wrong") {
		t.Fatal("password verification mismatch")
	}
}

func TestPasswordPreservesPunctuationUnicodeAndHashFileLineEnding(t *testing.T) {
	password := `!@#$%^&*()_+-=[]{}|;:',.<>/?~` + " tabs\tand unicode üU0001f510"
	h, err := HashPassword(password)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "password.phc")
	if err := os.WriteFile(path, []byte(h+"\r\n"), 0600); err != nil {
		t.Fatal(err)
	}
	loaded, err := readSecret("", path, 4096)
	if err != nil {
		t.Fatal(err)
	}
	if !verifyPassword(loaded, password) {
		t.Fatal("special-character password changed during hashing or file loading")
	}
}

func TestPasswordRejectsLineBreaks(t *testing.T) {
	for _, password := range []string{"line\nbreak", "line\rbreak", "line\r\nbreak"} {
		if _, err := HashPassword(password); err == nil {
			t.Fatalf("accepted %q", password)
		}
	}
}
func TestPasswordHashRejectsWeakerOrMalformedValues(t *testing.T) {
	valid, _ := HashPassword("password")
	for _, h := range []string{"", strings.Replace(valid, "m=19456", "m=4096", 1), strings.Replace(valid, "t=2", "t=1", 1), strings.Replace(valid, "p=1", "p=2", 1)} {
		if _, _, err := parsePasswordHash(h); err == nil {
			t.Fatalf("accepted %q", h)
		}
	}
}

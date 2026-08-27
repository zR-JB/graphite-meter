package auth

import (
	"bufio"
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"

	"golang.org/x/crypto/argon2"
	"golang.org/x/term"
)

const (
	argonMemory  uint32 = 19 * 1024
	argonTime    uint32 = 2
	argonThreads uint8  = 1
	argonSaltLen        = 16
	argonKeyLen         = 32
)

func HashPassword(password string) (string, error) {
	if err := validatePassword(password); err != nil {
		return "", err
	}
	salt := make([]byte, argonSaltLen)
	if _, err := rand.Read(salt); err != nil {
		return "", err
	}
	key := argon2.IDKey([]byte(password), salt, argonTime, argonMemory, argonThreads, argonKeyLen)
	b64 := base64.RawStdEncoding
	return fmt.Sprintf("$argon2id$v=19$m=%d,t=%d,p=%d$%s$%s", argonMemory, argonTime, argonThreads, b64.EncodeToString(salt), b64.EncodeToString(key)), nil
}

func parsePasswordHash(encoded string) ([]byte, []byte, error) {
	parts := strings.Split(strings.TrimSpace(encoded), "$")
	if len(parts) != 6 || parts[1] != "argon2id" || parts[2] != "v=19" {
		return nil, nil, errors.New("password hash must be an Argon2id v=19 PHC string")
	}
	params := strings.Split(parts[3], ",")
	if len(params) != 3 {
		return nil, nil, errors.New("password hash has invalid Argon2 parameters")
	}
	want := []struct {
		prefix string
		value  uint64
	}{{"m=", uint64(argonMemory)}, {"t=", uint64(argonTime)}, {"p=", uint64(argonThreads)}}
	for i, item := range want {
		if !strings.HasPrefix(params[i], item.prefix) {
			return nil, nil, errors.New("password hash has invalid Argon2 parameters")
		}
		n, err := strconv.ParseUint(strings.TrimPrefix(params[i], item.prefix), 10, 32)
		if err != nil || n != item.value {
			return nil, nil, fmt.Errorf("password hash must use m=%d,t=%d,p=%d", argonMemory, argonTime, argonThreads)
		}
	}
	b64 := base64.RawStdEncoding
	salt, err := b64.DecodeString(parts[4])
	if err != nil || len(salt) != argonSaltLen {
		return nil, nil, fmt.Errorf("password hash must use a %d-byte salt", argonSaltLen)
	}
	key, err := b64.DecodeString(parts[5])
	if err != nil || len(key) != argonKeyLen {
		return nil, nil, fmt.Errorf("password hash must use a %d-byte output", argonKeyLen)
	}
	return salt, key, nil
}

func verifyPassword(encoded, password string) bool {
	salt, expected, err := parsePasswordHash(encoded)
	if err != nil || validatePassword(password) != nil {
		return false
	}
	actual := argon2.IDKey([]byte(password), salt, argonTime, argonMemory, argonThreads, argonKeyLen)
	return subtle.ConstantTimeCompare(actual, expected) == 1
}

func validatePassword(password string) error {
	if len(password) == 0 || len(password) > 1024 {
		return errors.New("password must contain 1 to 1024 bytes")
	}
	if strings.ContainsAny(password, "\r\n") {
		return errors.New("password must not contain line breaks")
	}
	return nil
}

func ReadPassword(reader *bufio.Reader, input *os.File) (string, error) {
	if term.IsTerminal(int(input.Fd())) {
		value, err := term.ReadPassword(int(input.Fd()))
		fmt.Fprintln(os.Stderr)
		return string(value), err
	}
	value, err := reader.ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return "", err
	}
	return strings.TrimRight(value, "\r\n"), nil
}

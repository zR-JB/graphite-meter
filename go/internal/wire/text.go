package wire

import (
	"strings"
	"unicode"
	"unicode/utf8"
)

// SafeText reports whether s can reach a terminal as-is: valid UTF-8 without C0, DEL or C1 controls.
func SafeText(s string) bool {
	return utf8.ValidString(s) && !strings.ContainsFunc(s, unicode.IsControl)
}

// CleanText blanks controls, replaces invalid UTF-8 and keeps at most limit runes.
func CleanText(s string, limit int) string {
	s = strings.Map(func(r rune) rune {
		if unicode.IsControl(r) {
			return ' '
		}
		return r
	}, strings.ToValidUTF8(s, "�"))
	if utf8.RuneCountInString(s) > limit {
		s = string([]rune(s)[:max(limit-1, 0)]) + "…"
	}
	return s
}

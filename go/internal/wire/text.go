package wire

import (
	"strings"
	"unicode"
	"unicode/utf8"
)

// unsafe marks C0, DEL and C1 controls and the bidi controls that reorder surrounding text.
func unsafe(r rune) bool { return unicode.IsControl(r) || unicode.Is(unicode.Bidi_Control, r) }

// SafeText reports whether s can reach a terminal as-is: valid UTF-8 without controls or bidi overrides.
func SafeText(s string) bool {
	return utf8.ValidString(s) && !strings.ContainsFunc(s, unsafe)
}

// CleanText blanks controls and bidi overrides, replaces invalid UTF-8 and keeps at most limit runes.
func CleanText(s string, limit int) string {
	s = strings.Map(func(r rune) rune {
		if unsafe(r) {
			return ' '
		}
		return r
	}, strings.ToValidUTF8(s, "�"))
	if utf8.RuneCountInString(s) > limit {
		s = string([]rune(s)[:max(limit-1, 0)]) + "…"
	}
	return s
}

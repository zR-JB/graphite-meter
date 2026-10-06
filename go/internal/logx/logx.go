// Package logx writes the server's log lines: a local time, a level, a topic and the message, in aligned columns so
// a warning or a topic stands out when skimming, as the Rust server prints them. A terminal gets colour and a
// warning's or error's advice, after "; ", on its own help line.
package logx

import (
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode"
)

type Level int

const (
	Debug Level = iota
	Info
	Warn
	Error
)

var labels = [...]string{"DEBUG", "INFO", "WARN", "ERROR"}

// The label's and the message's terminal colours.
var colours = [...][2]string{{"34", "2"}, {"32", "0"}, {"1;33", "33"}, {"1;31", "1;31"}}

// Topics are padded to the longest, "discovery:", so messages start in one column.
const topicWidth = 10

var (
	mu     sync.Mutex
	out    io.Writer = os.Stderr
	colour           = sync.OnceValue(func() bool {
		set := func(name string) bool { v := os.Getenv(name); return v != "" && v != "0" }
		info, err := os.Stderr.Stat()
		terminal := err == nil && info.Mode()&os.ModeCharDevice != 0 && os.Getenv("TERM") != "dumb" &&
			os.Getenv("TERM") != ""
		return !set("NO_COLOR") && (set("FORCE_COLOR") || terminal)
	})
)

func Debugf(topic, format string, args ...any) { write(Debug, topic, fmt.Sprintf(format, args...)) }
func Infof(topic, format string, args ...any)  { write(Info, topic, fmt.Sprintf(format, args...)) }
func Warnf(topic, format string, args ...any)  { write(Warn, topic, fmt.Sprintf(format, args...)) }
func Errorf(topic, format string, args ...any) { write(Error, topic, fmt.Sprintf(format, args...)) }

func write(level Level, topic, message string) {
	line := Line(level, topic, message, time.Now(), colour())
	mu.Lock()
	defer mu.Unlock()
	_, _ = io.WriteString(out, line)
}

// SetOutput sends the lines to w, as tests read them, and returns what restores the last output.
func SetOutput(w io.Writer) (restore func()) {
	mu.Lock()
	defer mu.Unlock()
	last := out
	out = w
	return func() {
		mu.Lock()
		defer mu.Unlock()
		out = last
	}
}

// Writer takes another logger's lines, such as net/http's, at one level and topic.
func Writer(level Level, topic string) io.Writer { return writer{level, topic} }

type writer struct {
	level Level
	topic string
}

func (w writer) Write(b []byte) (int, error) {
	write(w.level, w.topic, strings.TrimSuffix(string(b), "\n"))
	return len(b), nil
}

// Line formats one line; control characters are escaped so no peer text drives a terminal.
func Line(level Level, topic, message string, at time.Time, colour bool) string {
	var text strings.Builder
	for _, r := range message {
		if unicode.IsControl(r) {
			quoted := strconv.QuoteRune(r)
			text.WriteString(quoted[1 : len(quoted)-1])
		} else {
			text.WriteRune(r)
		}
	}
	stamp := at.Format(time.RFC3339)
	tag := fmt.Sprintf("%-*s", topicWidth, topic+":")
	label := fmt.Sprintf("%-5s", labels[level])
	if !colour {
		return fmt.Sprintf("%s %s %s %s\n", stamp, label, tag, text.String())
	}
	what, advice, split := strings.Cut(text.String(), "; ")
	if !split || level < Warn {
		what, advice = text.String(), ""
	}
	badge, ink := colours[level][0], colours[level][1]
	line := fmt.Sprintf("\x1b[2m%s\x1b[0m \x1b[%sm%s\x1b[0m \x1b[1m%s\x1b[0m \x1b[%sm%s\x1b[0m\n",
		stamp, badge, label, tag, ink, what)
	if advice != "" {
		line += fmt.Sprintf("%*s\x1b[1;36mhelp:\x1b[0m %s\n", len(stamp)+8+topicWidth-len("help: "), "", advice)
	}
	return line
}

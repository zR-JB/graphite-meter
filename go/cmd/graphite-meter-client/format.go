package main

import (
	"crypto/tls"
	"errors"
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func errorText(err error) string {
	text := wire.CleanText(err.Error(), 320)
	if _, untrusted := errors.AsType[*tls.CertificateVerificationError](err); untrusted {
		text += " Turn on Skip TLS verify (-insecure) only for a server you trust."
	}
	return text
}

var rateUnits = []string{"bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"}

func fmtRate(bytesPerSec float64) string {
	bits := bytesPerSec * 8
	tier := 0
	for tier < len(rateUnits)-1 && bits >= 1.2*math.Pow(1000, float64(tier+1)) {
		tier++
	}
	return fmtSpeed(bits/math.Pow(1000, float64(tier))) + " " + rateUnits[tier]
}

func fmtSpeed(value float64) string {
	switch {
	case math.Round(value*10) >= 10_000:
		return strconv.FormatFloat(value, 'f', 0, 64)
	case math.Round(value*100) >= 10_000:
		return strconv.FormatFloat(value, 'f', 1, 64)
	}
	return strconv.FormatFloat(value, 'f', 2, 64)
}

func fmtBytes(n uint64) string {
	units := []string{"B", "kB", "MB", "GB", "TB"}
	value, tier := float64(n), 0
	for value >= 1000 && tier < len(units)-1 {
		value /= 1000
		tier++
	}
	if tier == 0 {
		return fmt.Sprintf("%d B", n)
	}
	return fmt.Sprintf("%.1f %s", value, units[tier])
}

func fmtMs(d time.Duration) string {
	ms := float64(d) / float64(time.Millisecond)
	if math.Abs(math.Round(ms*10)) < 1000 {
		return fmt.Sprintf("%.1f ms", ms)
	}
	return fmt.Sprintf("%.0f ms", ms)
}

func fmtAdded(d time.Duration) string {
	if d < 0 {
		return "−" + fmtMs(-d)
	}
	return "+" + fmtMs(d)
}

func fmtSetting(d time.Duration) string {
	if d < time.Second {
		return fmt.Sprintf("%d ms", d.Milliseconds())
	}
	return strconv.FormatFloat(d.Seconds(), 'f', -1, 64) + " s"
}

func fmtClock(d time.Duration) string {
	return fmt.Sprintf("%.1f s", max(d, 0).Seconds())
}

func latencyCells(s goclient.LatencyStats, idle *goclient.LatencyStats) []string {
	cells := []string{missing, "", missing, missing, missing}
	if s.Count > 0 {
		cells[0], cells[2] = fmtMs(s.P50), fmtMs(s.P95)
		if idle != nil && idle.Count > 0 {
			cells[1] = fmtAdded(s.P50 - idle.P50)
		}
	}
	if s.JitterPairs > 0 {
		cells[3] = fmtMs(s.Jitter)
	}
	if ratio, ok := s.TimeoutRatio(); ok {
		digits := 1
		if ratio > 0 && ratio < 0.01 {
			digits = 2
		}
		cells[4] = fmt.Sprintf("%d/%d (%.*f%%)", s.Timeouts, s.Count+s.Timeouts, digits, ratio*100)
	}
	return cells
}

func latencyFacts(s goclient.LatencyStats) []string {
	facts := []string{fmt.Sprintf("%d replies", s.Count)}
	if s.Elapsed > 0 {
		facts = append(facts, fmtClock(s.Elapsed))
	}
	if s.Unresolved > 0 {
		facts = append(facts, fmt.Sprintf("unfinished probes %d", s.Unresolved))
	}
	if s.SendFailures > 0 {
		facts = append(facts, fmt.Sprintf("failed sends %d", s.SendFailures))
	}
	return facts
}

func throughputFacts(r goclient.Result) []string {
	var facts []string
	if r.PeakBps > 0 {
		facts = append(facts, "peak "+fmtRate(r.PeakBps))
	}
	facts = append(facts, fmtBytes(r.TotalBytes))
	if r.Elapsed > 0 {
		facts = append(facts, fmtClock(r.Elapsed))
	}
	if r.Samples > 0 {
		facts = append(facts, fmt.Sprintf("%d samples", r.Samples))
	}
	if r.ReceiverTimed() {
		facts = append(facts, "receiver-timed")
	}
	return facts
}

func wrapParts(parts []string, w int) []string {
	var lines []string
	line := ""
	for _, part := range parts {
		switch {
		case line == "":
			line = part
		case len([]rune(line))+3+len([]rune(part)) <= w:
			line += " · " + part
		default:
			lines = append(lines, line)
			line = part
		}
	}
	return append(lines, line)
}

func reflectorTimingFacts(s *goclient.ReflectorTimingStats) (string, []string) {
	label := fmt.Sprintf("Server timing (%d paired replies, means)", s.Count)
	return label, []string{"raw " + fmtMs(s.MeanRawRTT), "handling " + fmtMs(s.MeanHandling)}
}

var eighths = []string{"", "▏", "▎", "▍", "▌", "▋", "▊", "▉"}

func (s styles) bar(value, scale float64, width int) string {
	cells := 0.0
	if scale > 0 {
		cells = min(max(value/scale*float64(width), 0), float64(width))
	}
	full := int(cells)
	part := eighths[int((cells-float64(full))*8)]
	rest := width - full
	if part != "" {
		rest--
	}
	return s.accent.Render(strings.Repeat("█", full)+part) + s.muted.Render(strings.Repeat("░", rest))
}

func pad(s string, w int) string {
	return s + strings.Repeat(" ", max(0, w-lipgloss.Width(s)))
}

func (s styles) checkbox(on bool) string {
	if on {
		return s.accent.Render("●")
	}
	return s.muted.Render("○")
}

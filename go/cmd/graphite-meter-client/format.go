package main

import (
	"crypto/tls"
	"errors"
	"fmt"
	"math"
	"net"
	"net/url"
	"strconv"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func errorText(err error) string {
	_, transport := errors.AsType[*url.Error](err)
	op, network := errors.AsType[*net.OpError](err)
	switch cert, untrusted := errors.AsType[*tls.CertificateVerificationError](err); {
	case untrusted:
		return "Certificate not trusted: " + wire.CleanText(strings.TrimPrefix(cert.Err.Error(), "x509: "), 200) +
			". Turn on Skip TLS verify (-insecure) only for a server you trust."
	case network && op.Op == "dial":
		return "Server could not be reached"
	case transport || network:
		return failureLabels[goclient.ReasonOf(err)]
	}
	return wire.CleanText(err.Error(), 320)
}

var rateUnits = []string{"bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"}

func rateTier(bits, headroom float64) (float64, string) {
	tier := 0
	for tier < len(rateUnits)-1 && bits >= headroom*math.Pow(1000, float64(tier+1)) {
		tier++
	}
	return bits / math.Pow(1000, float64(tier)), rateUnits[tier]
}

func fmtRate(bytesPerSec float64) string {
	v, unit := rateTier(bytesPerSec*8, 1.2)
	return fmtSpeed(v) + " " + unit
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

func fmtCount(n int) string {
	s := strconv.Itoa(n)
	for i := len(s) - 3; i > 0; i -= 3 {
		s = s[:i] + "," + s[i:]
	}
	return s
}

func fmtBytes(n uint64) string {
	units := []string{"B", "kB", "MB", "GB", "TB"}
	value, tier := float64(n), 0
	for math.Round(value*10) >= 10_000 && tier < len(units)-1 {
		value /= 1000
		tier++
	}
	if tier == 0 {
		return fmt.Sprintf("%d B", n)
	}
	return fmt.Sprintf("%.1f %s", value, units[tier])
}

func fixedMs(d time.Duration) string {
	ms := float64(d) / float64(time.Millisecond)
	if math.Abs(math.Round(ms*10)) < 1000 {
		return fmt.Sprintf("%.1f ms", ms)
	}
	return fmt.Sprintf("%.0f ms", ms)
}

func fmtMs(d time.Duration) string {
	if d >= 0 && d < 100*time.Microsecond {
		return "< 0.1 ms"
	}
	return fixedMs(d)
}

func fmtAdded(d time.Duration) string {
	if math.Round(float64(d)/float64(time.Millisecond)*10) < 0 {
		return "−" + fixedMs(-d)
	}
	return "+" + fixedMs(d.Abs())
}

// fmtSetting reads like the browser's times: ms, then seconds, then minutes, then hours in whole minutes.
func fmtSetting(d time.Duration) string {
	switch {
	case d < time.Second:
		return fmt.Sprintf("%d ms", d.Milliseconds())
	case d < time.Minute:
		return strconv.FormatFloat(d.Seconds(), 'f', -1, 64) + " s"
	}
	whole := d.Round(time.Second)
	large, small, unit, rest := whole/time.Minute, whole%time.Minute/time.Second, "min", "s"
	if d >= time.Hour {
		minutes := d.Round(time.Minute) / time.Minute
		large, small, unit, rest = minutes/60, minutes%60, "h", "min"
	}
	if small == 0 {
		return fmt.Sprintf("%d %s", large, unit)
	}
	return fmt.Sprintf("%d %s %d %s", large, unit, small, rest)
}

func fmtClock(d time.Duration) string {
	return fmt.Sprintf("%.1f s", max(d, 0).Seconds())
}

func latencyCells(population goclient.Result, idle *goclient.LatencyStats) []string {
	s := population.Latency
	cells := []string{missing, missing, missing, missing, missing}
	if population.HasMedian() {
		cells[0] = fmtMs(s.P50)
		if idle != nil {
			cells[1] = fmtAdded(s.P50 - idle.P50)
		}
	}
	if s.Count > 0 {
		cells[2] = fmtMs(s.P95)
	}
	if s.JitterPairs > 0 {
		cells[3] = fmtMs(s.Jitter)
	}
	if ratio, ok := s.TimeoutRatio(); ok {
		cells[4] = fmtCount(s.Timeouts) + " / " + fmtCount(s.Count+s.Timeouts)
		switch {
		case ratio >= 0.01:
			cells[4] += fmt.Sprintf(" (%.1f%%)", ratio*100)
		case ratio > 0:
			cells[4] += fmt.Sprintf(" (%.2f%%)", ratio*100)
		}
	}
	return cells
}

func latencyFacts(s goclient.LatencyStats) []string {
	facts := []string{fmtCount(s.Count) + " replies"}
	if s.Elapsed > 0 {
		facts = append(facts, fmtClock(s.Elapsed))
	}
	if s.Unresolved > 0 {
		facts = append(facts, "unfinished probes "+fmtCount(s.Unresolved))
	}
	if s.SendFailures > 0 {
		facts = append(facts, "failed sends "+fmtCount(s.SendFailures))
	}
	return facts
}

func throughputFacts(r goclient.Result, brief bool) []string {
	var facts []string
	if r.PeakBps > 0 {
		peak := fmtRate(r.PeakBps)
		if brief {
			_, unit := rateTier(r.MeanBps*8, 1.2)
			peak = strings.TrimSuffix(peak, " "+unit)
		}
		facts = append(facts, "peak "+peak)
	}
	facts = append(facts, fmtBytes(r.TotalBytes))
	if r.Elapsed > 0 {
		facts = append(facts, fmtClock(r.Elapsed))
	}
	if r.Samples > 0 && !brief {
		facts = append(facts, fmtCount(r.Samples)+" samples")
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
		case lipgloss.Width(line)+3+lipgloss.Width(part) <= w:
			line += " · " + part
		default:
			lines = append(lines, line)
			line = part
		}
	}
	return append(lines, line)
}

func reflectorTimingFacts(s *goclient.ReflectorTimingStats) (string, []string) {
	label := "Server timing (" + fmtCount(s.Count) + " paired replies, means)"
	return label, []string{"raw " + fmtMs(s.MeanRawRTT), "handling " + fmtMs(s.MeanHandling)}
}

var eighths = []string{"", "▏", "▎", "▍", "▌", "▋", "▊", "▉"}

func (s styles) bar(fill lipgloss.Style, value, scale float64, width int) string {
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
	return fill.Render(strings.Repeat("█", full)+part) + s.muted.Render(strings.Repeat("░", rest))
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

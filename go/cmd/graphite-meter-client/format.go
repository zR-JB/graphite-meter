package main

import (
	"charm.land/lipgloss/v2"
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const missing = "—"

// errorText applies the wire text policy: error messages may carry a remote peer's words.
func errorText(err error) string { return wire.CleanText(err.Error(), 320) }

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
	case value >= 1000:
		return strconv.FormatFloat(value, 'f', 0, 64)
	case value >= 100:
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

var stageLabels = map[goclient.Stage]string{
	goclient.StageLatency:       "Latency",
	goclient.StageDownload:      "Download",
	goclient.StageUpload:        "Upload",
	goclient.StageBidirectional: "Bidirectional",
}

func compactStage(stage goclient.Stage) string {
	if stage == goclient.StageBidirectional {
		return "Bi-dir"
	}
	return stageLabels[stage]
}

func populationLabel(stage goclient.Stage) string {
	if stage == goclient.StageLatency {
		return "Idle latency"
	}
	return "Loaded latency · " + compactStage(stage)
}

func compactPopulation(stage goclient.Stage) string {
	if stage == goclient.StageLatency {
		return "Idle"
	}
	return "Loaded " + strings.ToLower(compactStage(stage))
}

func directionLabel(r goclient.Result) string {
	if r.Stage != goclient.StageBidirectional {
		return stageLabels[r.Stage]
	}
	if r.Direction == goclient.Up {
		return "Bi-dir ↑"
	}
	return "Bi-dir ↓"
}

// latencyCells gives median, added latency, p95, jitter, and probe timeouts; missing data stays "—".
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

func reflectorTimingSummary(s *goclient.ReflectorTimingStats) string {
	if s == nil {
		return ""
	}
	const summary = "Server timing (%d paired replies, means): raw %s · handling %s · adjusted %s (handling removed)."
	return fmt.Sprintf(summary, s.Count, fmtMs(s.MeanRawRTT), fmtMs(s.MeanHandling), fmtMs(s.MeanAdjustedRTT))
}

func protocolChoiceLabel(protocol string) string {
	if protocol == "auto" {
		return "Automatic"
	}
	return goclient.ProtocolLabel(protocol)
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

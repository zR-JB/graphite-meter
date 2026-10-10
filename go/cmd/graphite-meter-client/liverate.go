package main

import (
	"math"
	"time"
)

// The live rate the browser presents (client/src/lib/runner/liveRates.ts), shared through
// api/liverate.testvectors.json: a growing average that restarts on a confirmed shift.
const (
	presentationMinWindowMS = 800.0
	presentationFirstMS     = 500.0
	fastWindowMS            = 750.0
	regimeReadyMS           = 2000.0
	regimeDownshiftMS       = 750.0
	regimeUpshiftMS         = 500.0
)

type rateSpan struct{ start, end, bytes, total float64 }

type regimeCandidate struct {
	up               bool
	start, reference float64
}

// liveRate presents a stage's rate from bytes over time; the first delivery only anchors it.
type liveRate struct {
	anchored                           bool
	spans                              []rateSpan
	head                               int
	bytes, evidence, regimeStart, fast float64
	candidate                          *regimeCandidate
	presented                          float64
}

// presentationWindowMS covers 85% of the current regime, never less than 800 ms.
func presentationWindowMS(age float64) float64 {
	age = math.Max(0, age)
	if math.IsNaN(age) {
		age = 0
	}
	return math.Min(age, math.Max(presentationMinWindowMS, age*0.85))
}

// observeRate adds a window's rate over its length and reports whether a rate is shown.
func (e *liveRate) observeRate(bytesPerSec float64, window time.Duration) bool {
	ms := float64(window) / float64(time.Millisecond)
	e.observe(bytesPerSec*ms/1000, ms)
	return e.presented > 0
}

// observe adds bytes measured over ms and reports whether a new regime was confirmed.
func (e *liveRate) observe(bytes, ms float64) bool {
	if !(ms > 0) || math.IsInf(ms, 0) {
		return false
	}
	if !e.anchored {
		e.anchored = bytes > 0
		return false
	}
	start := e.evidence
	e.evidence += ms
	if !(bytes > 0) {
		bytes = 0
	}
	e.bytes += bytes
	e.spans = append(e.spans, rateSpan{start, e.evidence, bytes, e.bytes})
	e.recalculate()
	changed := e.regime(start)
	if changed {
		e.recalculate()
	}
	keep := math.Min(e.evidence-presentationWindowMS(e.evidence-e.regimeStart),
		math.Max(e.regimeStart, e.evidence-fastWindowMS))
	if e.candidate != nil {
		keep = math.Min(keep, e.candidate.start)
	}
	for e.head < len(e.spans)-1 && e.spans[e.head].end <= keep {
		e.head++
	}
	if e.head >= 1024 && e.head*2 >= len(e.spans) {
		e.spans, e.head = append(e.spans[:0], e.spans[e.head:]...), 0
	}
	return changed
}

func (e *liveRate) recalculate() {
	if e.evidence < presentationFirstMS {
		e.presented = 0
	} else {
		e.presented = e.rateSince(e.evidence - presentationWindowMS(e.evidence-e.regimeStart))
	}
	e.fast = e.rateSince(math.Max(e.regimeStart, e.evidence-fastWindowMS))
}

// rateSince prorates the span the window starts in, so a window edge never rescans a long stage.
func (e *liveRate) rateSince(start float64) float64 {
	lo, hi := e.head, len(e.spans)-1
	for lo < hi {
		if mid := (lo + hi) / 2; e.spans[mid].end <= start {
			lo = mid + 1
		} else {
			hi = mid
		}
	}
	first := e.spans[lo]
	from := math.Max(start, first.start)
	ms := e.evidence - from
	if !(ms > 0) {
		return 0
	}
	partial := first.bytes * ((first.end - from) / (first.end - first.start))
	return ((e.bytes - first.total + partial) * 1000) / ms
}

func (e *liveRate) regime(start float64) bool {
	c := e.candidate
	if c == nil {
		if e.evidence-e.regimeStart < regimeReadyMS || e.presented <= 0 {
			return false
		}
		if ratio := e.fast / e.presented; ratio < 0.75 || ratio > 1.2 {
			e.candidate = &regimeCandidate{up: ratio > 1.2, start: start, reference: e.presented}
		}
		return false
	}
	ratio := 1.0
	if c.reference > 0 {
		ratio = e.fast / c.reference
	}
	if c.up && ratio < 1.1 || !c.up && ratio > 0.85 {
		e.candidate = nil
		return false
	}
	confirm := regimeDownshiftMS
	if c.up {
		confirm = regimeUpshiftMS
	}
	if e.evidence-c.start < confirm {
		return false
	}
	e.regimeStart, e.candidate = c.start, nil
	return true
}

package main

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// The printed report a finished run leaves in the terminal.

func serverLabel(name, location string) string {
	if location == "" {
		return name
	}
	return name + " · " + location
}

type results struct {
	throughput, latency string
	failures, notes     []string
	added               bool
}

func (r results) view() string {
	parts := append([]string{r.throughput, r.latency}, r.failures...)
	return strings.Join(slices.DeleteFunc(parts, func(part string) bool { return part == "" }), "\n")
}

func (m model) note(label string, facts []string, w int) []string {
	lines := []string{label + ":"}
	if first := label + ": " + facts[0]; lipgloss.Width(first) <= w-2 {
		lines, facts = nil, append([]string{first}, facts[1:]...)
	}
	for _, line := range wrapParts(facts, w-2) {
		if len(lines) > 0 {
			line = "  " + line
		}
		lines = append(lines, line)
	}
	for i, line := range lines {
		lines[i] = m.st.muted.Render(line)
	}
	return lines
}

func (r *runState) unmeasured(i int) string {
	return cmp.Or(stageStatusLabels[r.stages[i].state], missing)
}

func (r *runState) failureReason(stage goclient.Stage, err error) goclient.FailureReason {
	if r.details != nil && errors.Is(err, context.Canceled) {
		failures := r.details.Failures
		i := slices.IndexFunc(failures, func(f goclient.ServerFailure) bool {
			return f.Stage == stage && f.ServerID == r.latencyServer()
		})
		if i < 0 {
			i = slices.IndexFunc(failures, func(f goclient.ServerFailure) bool { return f.Stage == stage })
		}
		if i >= 0 {
			return failures[i].Reason
		}
	}
	return goclient.ReasonOf(err)
}

func (m model) resultsView(w int, latencyHeading string) results {
	r := m.run
	latency := r.latencyPopulations()
	var idle *goclient.LatencyStats
	if population, ok := latency[goclient.StageLatency]; ok && population.HasMedian() {
		idle = &population.Latency
	}
	var out results
	var throughput, latencyRows [][]string
	failed := func(stage goclient.Stage, label string, err error) {
		switch {
		case err == nil:
		case errors.Is(err, context.Canceled) && r.outcome == goclient.OutcomeStopped:
			out.failures = append(out.failures, m.st.warn.Render(label+" stopped."))
		default:
			out.failures = append(out.failures, m.st.err.Render(label+": "+failureLabels[r.failureReason(stage, err)]))
		}
	}
	measured := false
	for i, stage := range r.plan {
		hue := m.st.stage[stage.Name]
		if len(stage.Directions) > 0 {
			for _, result := range r.results {
				if result.Stage != stage.Name {
					continue
				}
				if !result.Unavailable || result.TotalBytes > 0 {
					out.notes = append(out.notes, m.note(directionLabel(result), throughputFacts(result, false), w)...)
				}
				failed(stage.Name, directionLabel(result), result.Err)
			}
			rates := r.meanRates(stage.Name)
			measured = measured || rates != ""
			switch {
			case rates == "" && !r.running():
				rates = r.unmeasured(i)
			case r.stages[i].state == stagePartial:
				rates += "  " + m.st.warn.Render(stageStatusLabels[stagePartial])
			}
			if rates != "" {
				throughput = append(throughput, []string{hue.Render(compactStage(stage.Name)), rates})
			}
		}
		population, ok := latency[stage.Name]
		switch {
		case ok:
			measured = true
			cells := latencyCells(population, idle)
			if stage.Name == goclient.StageLatency {
				cells[1] = ""
			}
			out.added = out.added || cells[1] != ""
			latencyRows = append(latencyRows, append([]string{hue.Render(compactPopulation(stage.Name))}, cells...))
			label := populationLabel(stage.Name)
			out.notes = append(out.notes, m.note(label, latencyFacts(population.Latency), w)...)
			if timing := population.Latency.ReflectorTiming; timing != nil {
				label, facts := reflectorTimingFacts(timing)
				out.notes = append(out.notes, m.note(label, facts, w)...)
			}
			failed(stage.Name, label, population.Err)
		case len(stage.Directions) == 0 && !r.running():
			latencyRows = append(latencyRows, []string{hue.Render(compactPopulation(stage.Name)), r.unmeasured(i)})
		}
	}
	if !measured {
		return results{}
	}
	if out.added {
		out.notes = append(out.notes, m.st.muted.Render(addedNote))
	}
	scope := ""
	if m.multipleRunServers() {
		scope = "All servers"
	}
	if len(throughput) > 0 {
		out.throughput = m.st.grid([]string{"Throughput", scope}, throughput, w)
	}
	if len(latencyRows) > 0 {
		headers := []string{latencyHeading, "Median", "Added", "P95", "Jitter", "Probe timeouts"}
		for i, row := range latencyRows {
			latencyRows[i] = append(row, make([]string, len(headers)-len(row))...)
			if !out.added {
				latencyRows[i] = slices.Delete(latencyRows[i], 2, 3)
			}
		}
		if !out.added {
			headers = slices.Delete(headers, 2, 3)
		}
		out.latency = m.st.grid(headers, latencyRows, w)
	}
	return out
}

func (m model) finalReport() string {
	if m.run == nil || m.running() {
		return ""
	}
	run := *m.run
	run.pick = ""
	m.run = &run
	w, _ := m.size()
	blocks := []string{m.reportHeader()}
	heading := "Latency"
	if m.multipleRunServers() {
		heading += " to " + m.serverName(m.run.latencyServer())
	}
	if results := m.resultsView(w, heading); results.view() != "" {
		blocks = append(blocks, m.throughputReport(w), results.latency, strings.Join(results.failures, "\n"),
			m.reportNotes(w, results.added))
	}
	if m.multipleRunServers() {
		blocks = append(blocks, m.detailsView(w, false))
	}
	if m.run.err != nil {
		blocks = append(blocks, m.st.err.Render(errorText(m.run.err)))
	}
	blocks = slices.DeleteFunc(blocks, func(block string) bool { return block == "" })
	report := strings.Split(strings.Join(blocks, "\n\n"), "\n")
	for i, line := range report {
		report[i] = strings.TrimRight(line, " ")
	}
	return strings.Join(report, "\n")
}

func (m model) reportHeader() string {
	r := m.run
	var facts []string
	switch {
	case r.details == nil:
	case len(r.details.Servers) == 1:
		facts = append(facts, r.details.Servers[0].Server.Name)
	default:
		facts = append(facts, fmt.Sprintf("%d servers", len(r.details.Servers)))
	}
	if !r.finished.IsZero() {
		facts = append(facts, fmtClock(r.finished.Sub(r.started)))
	}
	var total uint64
	for _, result := range r.results {
		total += result.TotalBytes
	}
	if total > 0 {
		facts = append(facts, fmtBytes(total))
	}
	tone := lipgloss.NewStyle().Bold(true).Foreground(m.st.outcome[r.outcome].GetBackground())
	header := m.st.heading.Render("Graphite Meter") + "  " + tone.Render(outcomeLabels[r.outcome])
	if len(facts) > 0 {
		header += "  " + m.st.muted.Render(strings.Join(facts, " · "))
	}
	return header
}

func (m model) throughputReport(w int) string {
	r := m.run
	type row struct {
		label, value, status string
		facts                []string
	}
	var rows []row
	labelW, valueW := 0, 0
	for i, stage := range r.plan {
		style := m.st.stage[stage.Name]
		for _, dir := range stage.Directions {
			row := row{label: style.Render(arrows[dir]) + " " + m.st.text.Render(stageLabels[stage.Name])}
			at := slices.IndexFunc(r.results, func(result goclient.Result) bool {
				return result.Stage == stage.Name && result.Direction == dir
			})
			switch {
			case at < 0:
				row.value = m.st.muted.Render(r.unmeasured(i))
			case r.results[at].Unavailable:
				row.value = m.st.muted.Render(missing)
			default:
				row.value = style.Bold(true).Render(fmtRate(r.results[at].MeanBps))
				row.facts = throughputFacts(r.results[at], true)
			}
			if r.stages[i].state == stagePartial {
				row.status = stageStatusLabels[stagePartial]
				row.facts = append([]string{row.status}, row.facts...)
			}
			labelW, valueW = max(labelW, lipgloss.Width(row.label)), max(valueW, lipgloss.Width(row.value))
			rows = append(rows, row)
		}
	}
	indent := labelW + valueW + 2
	var lines []string
	for _, row := range rows {
		line := pad(row.label, labelW) + "  " + pad(row.value, valueW)
		for i, facts := range wrapParts(row.facts, max(w-indent-3, 20)) {
			switch {
			case i > 0:
				line = strings.Repeat(" ", indent) + "   " + m.st.muted.Render(facts)
			case row.status != "":
				line += "   " + m.st.warn.Render(row.status) + m.st.muted.Render(strings.TrimPrefix(facts, row.status))
			case facts != "":
				line += "   " + m.st.muted.Render(facts)
			}
			lines = append(lines, line)
		}
	}
	return strings.Join(lines, "\n")
}

func (m model) reportNotes(w int, added bool) string {
	var notes, timing []string
	populations := m.run.latencyPopulations()
	for _, stage := range m.run.plan {
		population, ok := populations[stage.Name]
		if !ok {
			continue
		}
		s := population.Latency
		if population.Err != nil || s.Timeouts > 0 || s.Unresolved > 0 || s.SendFailures > 0 {
			notes = append(notes, m.note(populationLabel(stage.Name), latencyFacts(s), w)...)
		}
		if t := s.ReflectorTiming; t != nil {
			part := compactPopulation(stage.Name) + " " + fmtMs(t.MeanHandling) + " of " + fmtMs(t.MeanRawRTT)
			if t.Count != s.Count {
				part += " (" + fmtCount(t.Count) + " pairs)"
			}
			timing = append(timing, part)
		}
	}
	if len(timing) > 0 {
		notes = append(notes, m.note("Server handling of the mean round trip", timing, w)...)
	}
	if added {
		notes = append(notes, m.st.muted.Render(addedNote))
	}
	return strings.Join(notes, "\n")
}

// detailsView is every server's rates and latency medians, its issues and, when full, its intervals.
func (m model) detailsView(w int, full bool) string {
	r := m.run
	if r == nil || r.details == nil {
		return m.st.muted.Render("Waiting for the first server report…")
	}
	details := r.details
	headers := []string{"Server"}
	var columns []goclient.Result
	for _, stage := range r.plan {
		for _, dir := range stage.Directions {
			columns = append(columns, goclient.Result{Stage: stage.Name, Direction: dir})
			headers = append(headers, directionLabel(columns[len(columns)-1]))
		}
	}
	row := func(name string, results []goclient.Result) []string {
		cells := []string{name}
		for _, column := range columns {
			value := missing
			for _, r := range results {
				if r.Stage == column.Stage && r.Direction == column.Direction && !r.Unavailable {
					value = fmtRate(r.MeanBps)
				}
			}
			cells = append(cells, value)
		}
		return cells
	}
	rows := [][]string{row("All servers", r.results)}
	latency := [][]string{}
	for _, server := range details.Servers {
		name := server.Server.Name
		if !slices.Contains(details.Participants, server.Server.ID) {
			name += " ✗"
		}
		rows = append(rows, row(name, server.Results))
		cells := []string{server.Server.Name}
		for _, stage := range r.plan {
			value := missing
			for _, result := range server.Results {
				if result.Stage == stage.Name && result.Direction == "" && result.HasMedian() {
					value = fmtMs(result.Latency.P50)
				}
			}
			cells = append(cells, value)
		}
		latency = append(latency, cells)
	}
	populations := []string{"Server"}
	for _, stage := range r.plan {
		populations = append(populations, compactPopulation(stage.Name))
	}
	lines := []string{m.st.heading.Render(m.outcomeNotice())}
	if notes := m.resultsView(w, "Latency").notes; full && len(notes) > 0 {
		lines = append(append(lines, notes...), "")
	}
	lines = append(lines, m.st.grid(headers, rows, w), "",
		m.st.heading.Render("Latency median by server"), m.st.grid(populations, latency, w))
	if len(details.Failures) > 0 {
		lines = append(lines, "", m.st.heading.Render("Issues"))
		for _, f := range details.Failures {
			lines = append(lines, fmt.Sprintf("%s · %s %s · at %s · %s", m.serverName(f.ServerID),
				compactStage(f.Stage), f.Scope, fmtClock(f.At), failureLabels[f.Reason]))
		}
	}
	if full && details.Outcome != goclient.OutcomeRunning && len(details.Intervals) > 0 {
		lines = append(lines, "", m.st.heading.Render("Aggregation intervals"))
		for _, interval := range details.Intervals {
			state := "incomplete evidence"
			if interval.Complete && interval.Window != nil {
				state = "measured window"
			}
			names := make([]string, len(interval.Participants))
			for i, id := range interval.Participants {
				names[i] = m.serverName(id)
			}
			parts := []string{fmt.Sprintf("%s %.1f–%.1f s", compactStage(interval.Stage), interval.Start.Seconds(),
				interval.End.Seconds()), strings.Join(names, ", "), state}
			parts = slices.DeleteFunc(parts, func(part string) bool { return part == "" })
			lines = append(lines, m.st.muted.Render(strings.Join(parts, " · ")))
		}
		if details.OmittedIntervals > 0 {
			lines = append(lines, m.st.muted.Render(fmt.Sprintf(
				"%d older intervals omitted; byte totals retain the full run", details.OmittedIntervals)))
		}
	}
	return fit(strings.Join(lines, "\n"), w)
}

func (m model) outcomeNotice() string {
	details := m.run.details
	remaining, selected := len(details.Participants), len(details.Servers)
	switch {
	case selected == 1:
		label, _ := m.status()
		return label
	case m.run.outcome == goclient.OutcomeRunning && remaining < selected:
		return fmt.Sprintf("%d of %d servers remaining", remaining, selected)
	case m.run.outcome == goclient.OutcomeRunning:
		return fmt.Sprintf("All %d servers", selected)
	case remaining < selected:
		return fmt.Sprintf("%s · %d of %d servers", outcomeLabels[m.run.outcome], remaining, selected)
	}
	return fmt.Sprintf("%s · all %d servers", outcomeLabels[m.run.outcome], selected)
}

// fit cuts each line of s to w cells.
func fit(s string, w int) string {
	lines := strings.Split(s, "\n")
	for i, line := range lines {
		lines[i] = ansi.Truncate(line, max(w, 1), "…")
	}
	return strings.Join(lines, "\n")
}

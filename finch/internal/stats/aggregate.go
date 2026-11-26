package stats

import (
	"encoding/csv"
	"fmt"
	"io"
	"math"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

// RangeKind controls the time filter window.
type RangeKind int

const (
	RangeToday RangeKind = iota
	Range7D
	Range30D
	RangeAll
)

func (r RangeKind) Label() string {
	switch r {
	case RangeToday:
		return "Today"
	case Range7D:
		return "7 Days"
	case Range30D:
		return "30 Days"
	default:
		return "All Time"
	}
}

// MetricKind controls chart / heatmap / ranking values.
type MetricKind int

const (
	MetricCost MetricKind = iota
	MetricTokens
	MetricRequests
)

func (m MetricKind) Label() string {
	switch m {
	case MetricCost:
		return "Cost ($)"
	case MetricTokens:
		return "Tokens"
	default:
		return "Requests"
	}
}

// SplitKind controls how trend bars are stacked and ranked.
type SplitKind int

const (
	SplitAgent SplitKind = iota
	SplitModel
)

func (s SplitKind) Label() string {
	if s == SplitModel {
		return "Model"
	}
	return "Agent"
}

// Filter describes active dashboard filters.
type Filter struct {
	Range RangeKind
	Agent model.Agent // "" means all
	Model string      // "" means all
	Query string      // substring match on model/agent/project/session
	Now   time.Time
}

// KPI holds top-level summary metrics including speed & latency.
type KPI struct {
	TotalCostUSD   float64
	TotalTokens    int64
	InputTokens    int64
	OutputTokens   int64
	CacheRead      int64
	CacheWrite     int64
	Requests       int
	Sessions       int
	CacheHitRatio  float64 // CacheRead / (Input + CacheRead + CacheWrite)
	UnpricedCalls  int
	UnpricedModels []string

	// Speed & latency summary across calls with timing data.
	WeightedTPS  float64 // sum(output) / sum(gen_sec)
	MedianTPS    float64
	MedianTTFTMs float64
	P95TTFTMs    float64
	MedianDurMs  float64
	SpeedSamples int
	TTFTSamples  int
}

// ModelSpeed summarizes throughput (TPS) and latency (TTFT / Duration) per model.
type ModelSpeed struct {
	Model        string
	Samples      int
	TTFTSamples  int
	WeightedTPS  float64
	MedianTPS    float64
	MedianTTFTMs float64
	P95TTFTMs    float64
	MedianDurMs  float64
}

// RankItem summarizes a single model or agent in the ranking list.
type RankItem struct {
	Key       string
	Label     string
	CostUSD   float64
	Tokens    int64
	Requests  int
	MatchKind model.MatchKind
	MatchedAs string
	Share     float64
}

// TrendBucket is one time column (hour for Today, day otherwise).
type TrendBucket struct {
	Key    string // YYYY-MM-DD or HH:00
	Label  string
	Values map[string]float64 // keyed by Split series name
	Total  float64
}

// HeatmapDay is one square in the 53-week calendar grid.
type HeatmapDay struct {
	Date     time.Time
	DateKey  string
	CostUSD  float64
	Tokens   int64
	Requests int
}

// Snapshot is the computed view model consumed by the GUI.
type Snapshot struct {
	KPI         KPI
	Filtered    []model.Call
	SeriesNames []string // Top N series keys for stacked chart
	Buckets     []TrendBucket
	Rankings    []RankItem
	Speeds      []ModelSpeed
	Heatmap     []HeatmapDay // 53 * 7 days ending at today's week
	AllModels   []string
	AllAgents   []model.Agent
}

type speedAccum struct {
	outTokens int64
	genMs     int64
	tpsVals   []float64
	ttftVals  []float64
	durVals   []float64
}

func (a *speedAccum) add(c model.Call) {
	if c.DurationMs >= 100 {
		a.durVals = append(a.durVals, float64(c.DurationMs))
	}
	if c.TTFTMs > 0 && c.TTFTMs < 300_000 {
		a.ttftVals = append(a.ttftVals, float64(c.TTFTMs))
	}
	if tps := c.TPS(); tps > 0 {
		genMs := c.DurationMs
		if c.Source == "magpie" && c.TTFTMs > 0 && c.DurationMs > c.TTFTMs+100 {
			genMs = c.DurationMs - c.TTFTMs
		}
		a.outTokens += c.Tokens.Output
		a.genMs += genMs
		a.tpsVals = append(a.tpsVals, tps)
	}
}

// Compute builds a Snapshot from priced calls and active filters.
func Compute(all []model.Call, f Filter, metric MetricKind, split SplitKind) Snapshot {
	now := f.Now
	if now.IsZero() {
		now = time.Now()
	}
	loc := now.Location()
	todayStart := time.Date(now.Year(), now.Month(), now.Day(), 0, 0, 0, 0, loc)

	var cutoff time.Time
	switch f.Range {
	case RangeToday:
		cutoff = todayStart
	case Range7D:
		cutoff = todayStart.AddDate(0, 0, -6)
	case Range30D:
		cutoff = todayStart.AddDate(0, 0, -29)
	}

	modelSet := make(map[string]struct{})
	agentSet := make(map[model.Agent]struct{})
	q := strings.ToLower(strings.TrimSpace(f.Query))

	heatByDate := make(map[string]*HeatmapDay)
	filtered := make([]model.Call, 0, len(all))
	for _, c := range all {
		if c.Model != "" {
			modelSet[c.Model] = struct{}{}
		}
		agentSet[c.Agent] = struct{}{}

		if f.Agent != "" && c.Agent != f.Agent {
			continue
		}
		if f.Model != "" && c.Model != f.Model {
			continue
		}
		if q != "" && !matchesQuery(c, q) {
			continue
		}

		localTS := c.Timestamp.In(loc)
		dk := localTS.Format("2006-01-02")
		hd := heatByDate[dk]
		if hd == nil {
			hd = &HeatmapDay{DateKey: dk}
			heatByDate[dk] = hd
		}
		hd.CostUSD += c.CostUSD
		hd.Tokens += c.Tokens.Total()
		hd.Requests++

		if !cutoff.IsZero() && localTS.Before(cutoff) {
			continue
		}
		filtered = append(filtered, c)
	}

	var kpi KPI
	var overallSpeed speedAccum
	modelSpeeds := make(map[string]*speedAccum)
	sessions := make(map[string]struct{})
	unpricedSet := make(map[string]struct{})
	rankMap := make(map[string]*RankItem)

	for _, c := range filtered {
		kpi.TotalCostUSD += c.CostUSD
		kpi.InputTokens += c.Tokens.Input
		kpi.OutputTokens += c.Tokens.Output
		kpi.CacheRead += c.Tokens.CacheRead
		kpi.CacheWrite += c.Tokens.CacheWrite
		kpi.TotalTokens += c.Tokens.Total()
		kpi.Requests++
		if c.SessionID != "" {
			sessions[string(c.Agent)+":"+c.SessionID] = struct{}{}
		}
		if c.MatchKind == model.MatchNone {
			kpi.UnpricedCalls++
			if c.Model != "" {
				unpricedSet[c.Model] = struct{}{}
			}
		}

		overallSpeed.add(c)
		if c.Model != "" {
			ms := modelSpeeds[c.Model]
			if ms == nil {
				ms = &speedAccum{}
				modelSpeeds[c.Model] = ms
			}
			ms.add(c)
		}

		rk := seriesKey(c, split)
		ri := rankMap[rk]
		if ri == nil {
			lbl := rk
			if split == SplitAgent {
				lbl = model.AgentLabel(c.Agent)
			}
			ri = &RankItem{
				Key:       rk,
				Label:     lbl,
				MatchKind: c.MatchKind,
				MatchedAs: c.MatchedAs,
			}
			rankMap[rk] = ri
		}
		ri.CostUSD += c.CostUSD
		ri.Tokens += c.Tokens.Total()
		ri.Requests++
	}

	kpi.Sessions = len(sessions)
	if denom := kpi.InputTokens + kpi.CacheRead + kpi.CacheWrite; denom > 0 {
		kpi.CacheHitRatio = float64(kpi.CacheRead) / float64(denom)
	}
	if overallSpeed.genMs > 0 {
		kpi.WeightedTPS = float64(overallSpeed.outTokens) / (float64(overallSpeed.genMs) / 1000.0)
	}
	kpi.MedianTPS = percentile(overallSpeed.tpsVals, 0.50)
	kpi.MedianTTFTMs = percentile(overallSpeed.ttftVals, 0.50)
	kpi.P95TTFTMs = percentile(overallSpeed.ttftVals, 0.95)
	kpi.MedianDurMs = percentile(overallSpeed.durVals, 0.50)
	kpi.SpeedSamples = len(overallSpeed.tpsVals)
	kpi.TTFTSamples = len(overallSpeed.ttftVals)

	for m := range unpricedSet {
		kpi.UnpricedModels = append(kpi.UnpricedModels, m)
	}
	sort.Strings(kpi.UnpricedModels)

	speeds := make([]ModelSpeed, 0, len(modelSpeeds))
	for m, acc := range modelSpeeds {
		if len(acc.tpsVals) == 0 && len(acc.ttftVals) == 0 {
			continue
		}
		var wtps float64
		if acc.genMs > 0 {
			wtps = float64(acc.outTokens) / (float64(acc.genMs) / 1000.0)
		}
		speeds = append(speeds, ModelSpeed{
			Model:        m,
			Samples:      len(acc.tpsVals),
			TTFTSamples:  len(acc.ttftVals),
			WeightedTPS:  wtps,
			MedianTPS:    percentile(acc.tpsVals, 0.50),
			MedianTTFTMs: percentile(acc.ttftVals, 0.50),
			P95TTFTMs:    percentile(acc.ttftVals, 0.95),
			MedianDurMs:  percentile(acc.durVals, 0.50),
		})
	}
	sort.Slice(speeds, func(i, j int) bool {
		if speeds[i].Samples != speeds[j].Samples {
			return speeds[i].Samples > speeds[j].Samples
		}
		return speeds[i].WeightedTPS > speeds[j].WeightedTPS
	})

	rankings := make([]RankItem, 0, len(rankMap))
	var rankTotal float64
	for _, ri := range rankMap {
		rankings = append(rankings, *ri)
		rankTotal += rankMetric(*ri, metric)
	}
	sort.Slice(rankings, func(i, j int) bool {
		vi, vj := rankMetric(rankings[i], metric), rankMetric(rankings[j], metric)
		if vi != vj {
			return vi > vj
		}
		return rankings[i].Key < rankings[j].Key
	})
	if rankTotal > 0 {
		for i := range rankings {
			rankings[i].Share = rankMetric(rankings[i], metric) / rankTotal
		}
	}

	const maxSeries = 5
	topSeries := make(map[string]bool)
	var seriesNames []string
	for i, r := range rankings {
		if i < maxSeries {
			topSeries[r.Key] = true
			seriesNames = append(seriesNames, r.Key)
		}
	}
	if len(rankings) > maxSeries {
		seriesNames = append(seriesNames, "Other")
	}

	buckets := buildBuckets(filtered, f.Range, todayStart, loc, metric, split, topSeries)
	heatmap := buildHeatmapGrid(todayStart, heatByDate)

	allModels := make([]string, 0, len(modelSet))
	for m := range modelSet {
		allModels = append(allModels, m)
	}
	sort.Strings(allModels)

	allAgents := make([]model.Agent, 0, len(agentSet))
	for _, a := range model.AllAgents {
		if _, ok := agentSet[a]; ok {
			allAgents = append(allAgents, a)
		}
	}

	return Snapshot{
		KPI:         kpi,
		Filtered:    filtered,
		SeriesNames: seriesNames,
		Buckets:     buckets,
		Rankings:    rankings,
		Speeds:      speeds,
		Heatmap:     heatmap,
		AllModels:   allModels,
		AllAgents:   allAgents,
	}
}

func percentile(vals []float64, p float64) float64 {
	if len(vals) == 0 {
		return 0
	}
	cp := append([]float64(nil), vals...)
	sort.Float64s(cp)
	if len(cp) == 1 {
		return cp[0]
	}
	idx := p * float64(len(cp)-1)
	lo := int(math.Floor(idx))
	hi := int(math.Ceil(idx))
	if lo == hi {
		return cp[lo]
	}
	frac := idx - float64(lo)
	return cp[lo]*(1-frac) + cp[hi]*frac
}

func buildBuckets(
	calls []model.Call,
	rng RangeKind,
	todayStart time.Time,
	loc *time.Location,
	metric MetricKind,
	split SplitKind,
	topSeries map[string]bool,
) []TrendBucket {
	if rng == RangeToday {
		buckets := make([]TrendBucket, 24)
		for h := range 24 {
			k := fmt.Sprintf("%02d", h)
			buckets[h] = TrendBucket{
				Key:    k,
				Label:  fmt.Sprintf("%02d:00", h),
				Values: make(map[string]float64),
			}
		}
		for _, c := range calls {
			h := c.Timestamp.In(loc).Hour()
			sk := bucketSeries(c, split, topSeries)
			v := callMetric(c, metric)
			buckets[h].Values[sk] += v
			buckets[h].Total += v
		}
		return buckets
	}

	var startDay time.Time
	switch rng {
	case Range7D:
		startDay = todayStart.AddDate(0, 0, -6)
	case Range30D:
		startDay = todayStart.AddDate(0, 0, -29)
	default:
		startDay = todayStart.AddDate(0, 0, -29)
		for _, c := range calls {
			d := c.Timestamp.In(loc)
			day := time.Date(d.Year(), d.Month(), d.Day(), 0, 0, 0, 0, loc)
			if day.Before(startDay) {
				startDay = day
			}
		}
		if minStart := todayStart.AddDate(0, 0, -89); startDay.Before(minStart) {
			startDay = minStart
		}
	}

	var buckets []TrendBucket
	byKey := make(map[string]int)
	for d := startDay; !d.After(todayStart); d = d.AddDate(0, 0, 1) {
		k := d.Format("2006-01-02")
		byKey[k] = len(buckets)
		buckets = append(buckets, TrendBucket{
			Key:    k,
			Label:  d.Format("01/02"),
			Values: make(map[string]float64),
		})
	}
	for _, c := range calls {
		k := c.Timestamp.In(loc).Format("2006-01-02")
		idx, ok := byKey[k]
		if !ok {
			continue
		}
		sk := bucketSeries(c, split, topSeries)
		v := callMetric(c, metric)
		buckets[idx].Values[sk] += v
		buckets[idx].Total += v
	}
	return buckets
}

func buildHeatmapGrid(todayStart time.Time, byDate map[string]*HeatmapDay) []HeatmapDay {
	wd := int(todayStart.Weekday())
	endSat := todayStart.AddDate(0, 0, 6-wd)
	startSun := endSat.AddDate(0, 0, -(53*7 - 1))

	grid := make([]HeatmapDay, 0, 53*7)
	for d := startSun; !d.After(endSat); d = d.AddDate(0, 0, 1) {
		dk := d.Format("2006-01-02")
		day := HeatmapDay{Date: d, DateKey: dk}
		if existing, ok := byDate[dk]; ok {
			day.CostUSD = existing.CostUSD
			day.Tokens = existing.Tokens
			day.Requests = existing.Requests
		}
		grid = append(grid, day)
	}
	return grid
}

func seriesKey(c model.Call, split SplitKind) string {
	if split == SplitModel {
		if c.Model == "" {
			return "unknown"
		}
		return c.Model
	}
	return string(c.Agent)
}

func bucketSeries(c model.Call, split SplitKind, top map[string]bool) string {
	k := seriesKey(c, split)
	if top[k] {
		return k
	}
	return "Other"
}

func callMetric(c model.Call, m MetricKind) float64 {
	switch m {
	case MetricCost:
		return c.CostUSD
	case MetricTokens:
		return float64(c.Tokens.Total())
	default:
		return 1
	}
}

func rankMetric(r RankItem, m MetricKind) float64 {
	switch m {
	case MetricCost:
		return r.CostUSD
	case MetricTokens:
		return float64(r.Tokens)
	default:
		return float64(r.Requests)
	}
}

func matchesQuery(c model.Call, q string) bool {
	return strings.Contains(strings.ToLower(c.Model), q) ||
		strings.Contains(strings.ToLower(string(c.Agent)), q) ||
		strings.Contains(strings.ToLower(c.Project), q) ||
		strings.Contains(strings.ToLower(c.SessionID), q) ||
		strings.Contains(strings.ToLower(c.MatchedAs), q)
}

// ExportCSV writes calls in CSV format including timing and speed columns.
func ExportCSV(w io.Writer, calls []model.Call) error {
	cw := csv.NewWriter(w)
	if err := cw.Write([]string{
		"timestamp", "agent", "source", "project", "session_id", "model",
		"matched_as", "match_kind", "input_tokens", "output_tokens",
		"cache_read_tokens", "cache_write_tokens", "total_tokens",
		"ttft_ms", "duration_ms", "tps", "cost_usd",
	}); err != nil {
		return err
	}
	for _, c := range calls {
		if err := cw.Write([]string{
			c.Timestamp.Format(time.RFC3339),
			string(c.Agent),
			c.Source,
			c.Project,
			c.SessionID,
			c.Model,
			c.MatchedAs,
			c.MatchKind.String(),
			strconv.FormatInt(c.Tokens.Input, 10),
			strconv.FormatInt(c.Tokens.Output, 10),
			strconv.FormatInt(c.Tokens.CacheRead, 10),
			strconv.FormatInt(c.Tokens.CacheWrite, 10),
			strconv.FormatInt(c.Tokens.Total(), 10),
			strconv.FormatInt(c.TTFTMs, 10),
			strconv.FormatInt(c.DurationMs, 10),
			fmt.Sprintf("%.1f", c.TPS()),
			fmt.Sprintf("%.6f", c.CostUSD),
		}); err != nil {
			return err
		}
	}
	cw.Flush()
	return cw.Error()
}

// FormatTokens shortens large token counts cleanly (e.g. 1.24M, 48.2K).
func FormatTokens(n int64) string {
	switch {
	case n >= 1_000_000_000:
		return fmt.Sprintf("%.2fB", float64(n)/1e9)
	case n >= 1_000_000:
		return fmt.Sprintf("%.2fM", float64(n)/1e6)
	case n >= 1_000:
		return fmt.Sprintf("%.1fK", float64(n)/1e3)
	default:
		return strconv.FormatInt(n, 10)
	}
}

// FormatUSD formats a USD value nicely across orders of magnitude.
func FormatUSD(v float64) string {
	switch {
	case v == 0:
		return "$0.00"
	case v < 0.01:
		return fmt.Sprintf("$%.4f", v)
	case v < 1000:
		return fmt.Sprintf("$%.2f", v)
	default:
		return "$" + FormatInt(int64(math.Round(v)))
	}
}

// FormatInt formats an integer with thousands commas (e.g. 12,251).
func FormatInt(n int64) string {
	neg := n < 0
	if neg {
		n = -n
	}
	s := strconv.FormatInt(n, 10)
	if len(s) <= 3 {
		if neg {
			return "-" + s
		}
		return s
	}
	var b strings.Builder
	if neg {
		b.WriteByte('-')
	}
	rem := len(s) % 3
	if rem > 0 {
		b.WriteString(s[:rem])
		b.WriteByte(',')
	}
	for i := rem; i < len(s); i += 3 {
		if i > rem {
			b.WriteByte(',')
		}
		b.WriteString(s[i : i+3])
	}
	return b.String()
}

// FormatTPS formats tokens per second.
func FormatTPS(v float64) string {
	if v <= 0 {
		return "—"
	}
	if v >= 100 {
		return fmt.Sprintf("%.0f tok/s", v)
	}
	return fmt.Sprintf("%.1f tok/s", v)
}

// FormatMs formats latency/duration in ms or seconds.
func FormatMs(ms float64) string {
	if ms <= 0 {
		return "—"
	}
	if ms < 1000 {
		return fmt.Sprintf("%.0f ms", ms)
	}
	if ms < 60_000 {
		return fmt.Sprintf("%.2f s", ms/1000.0)
	}
	return fmt.Sprintf("%.1f m", ms/60_000.0)
}

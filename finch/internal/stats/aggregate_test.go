package stats

import (
	"math"
	"testing"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

func TestComputeSpeedAndAggregates(t *testing.T) {
	now := time.Date(2026, 11, 26, 14, 0, 0, 0, time.UTC)
	calls := []model.Call{
		{
			ID: "1", Timestamp: now.Add(-1 * time.Hour), Agent: model.AgentAntigravity, Source: "magpie",
			SessionID: "s1", Model: "gemini-3.8-flash-n",
			Tokens: model.Tokens{Input: 1000, Output: 500, CacheRead: 3000},
			TTFTMs: 500, DurationMs: 5500, CostUSD: 0.01, MatchKind: model.MatchSuffix,
		},
		{
			ID: "2", Timestamp: now.Add(-2 * time.Hour), Agent: model.AgentClaude, Source: "claude",
			SessionID: "s2", Model: "claude-opus-5-5",
			Tokens: model.Tokens{Input: 2000, Output: 400, CacheRead: 6000},
			TTFTMs: 1000, DurationMs: 8000, CostUSD: 0.12, MatchKind: model.MatchExact,
		},
	}

	snap := Compute(calls, Filter{Range: Range7D, Now: now}, MetricCost, SplitAgent)
	if snap.KPI.Requests != 2 || snap.KPI.Sessions != 2 {
		t.Fatalf("unexpected KPI counts: %+v", snap.KPI)
	}
	// Call 1 (magpie gateway): 500 out / (5.5s - 0.5s) = 100 tok/s
	// Call 2 (claude local):   400 out / 8.0s = 50 tok/s
	// Median: 75 tok/s
	if math.Abs(snap.KPI.MedianTPS-75.0) > 1e-6 {
		t.Fatalf("MedianTPS = %v, want 75", snap.KPI.MedianTPS)
	}
	if math.Abs(snap.KPI.MedianTTFTMs-750.0) > 1e-6 {
		t.Fatalf("MedianTTFTMs = %v, want 750", snap.KPI.MedianTTFTMs)
	}
	if len(snap.Speeds) != 2 {
		t.Fatalf("expected 2 model speed rows, got %d", len(snap.Speeds))
	}
	if len(snap.Heatmap) != 53*7 {
		t.Fatalf("expected 371 heatmap days, got %d", len(snap.Heatmap))
	}
}

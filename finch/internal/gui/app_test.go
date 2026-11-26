package gui

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
	"github.com/Konakonai/lilya-tools/finch/internal/pricing"
	"github.com/Konakonai/lilya-tools/finch/internal/scanner"
)

func TestAppHeadlessRenderAllTabs(t *testing.T) {
	tmp := t.TempDir()
	sc := scanner.New(scanner.Options{HomeDir: tmp, CacheDBPath: filepath.Join(tmp, "idx.db")})
	cat := pricing.NewCatalog(filepath.Join(tmp, "prices.json"), filepath.Join(tmp, "ov.json"))
	app := NewApp(sc, cat)

	now := time.Now()
	app.SetData([]model.Call{
		{
			ID: "1", Timestamp: now.Add(-1 * time.Hour), Agent: model.AgentClaude,
			SessionID: "s1", Project: "nixos-config", Model: "claude-opus-5-5",
			Tokens: model.Tokens{Input: 4000, Output: 1200, CacheRead: 42000, CacheWrite: 3000},
			TTFTMs: 920, DurationMs: 14200, CostUSD: 0.14, MatchKind: model.MatchExact,
		},
		{
			ID: "2", Timestamp: now.Add(-3 * time.Hour), Agent: model.AgentAntigravity,
			SessionID: "s2", Project: "lilya-tools", Model: "gemini-3.8-flash-n",
			Tokens: model.Tokens{Input: 2000, Output: 850, CacheRead: 12000},
			TTFTMs: 410, DurationMs: 4200, CostUSD: 0.005, MatchKind: model.MatchSuffix,
		},
	}, scanner.ScanSummary{FilesTotal: 2, CallsTotal: 2})

	tester := ui.NewTester(app.View, 1280, 860)
	// Overview tab
	tester.Frame()
	// Requests tab
	app.tab = tabRequests
	tester.Frame()
	// Pricing tab
	app.tab = tabPricing
	tester.Frame()
}

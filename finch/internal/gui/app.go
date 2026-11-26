package gui

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/chart"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
	"github.com/Konakonai/lilya-tools/finch/internal/pricing"
	"github.com/Konakonai/lilya-tools/finch/internal/scanner"
	"github.com/Konakonai/lilya-tools/finch/internal/stats"
)

const (
	tabOverview = 0
	tabRequests = 1
	tabPricing  = 2
)

// App holds the Finch desktop application state.
type App struct {
	scanner *scanner.Scanner
	catalog *pricing.Catalog
	theme   *ui.Theme

	mu       sync.Mutex
	uiCtx    *ui.Context
	allCalls []model.Call
	scanSum  scanner.ScanSummary
	loading  bool
	syncing  bool
	status   string

	// View controls (UI thread)
	tab          int
	rangeIdx     int // 0=Today, 1=7D, 2=30D, 3=All
	metricIdx    int // 0=Cost, 1=Tokens, 2=Requests
	splitIdx     int // 0=Agent, 1=Model
	agentOpt     string
	modelOpt     string
	query        string
	dirtySnap    bool
	snap         stats.Snapshot
	sortedReqs   []model.Call
	sortedSpeeds []stats.ModelSpeed
	speedSortCol string
	speedSortAsc bool
	reqTable     ui.ListState
	reqSort      ui.SortOrder
	reqSel       int
	priceTable   ui.ListState
	priceRows    []priceSummaryRow

	// Override editor modal state
	ovOpen       bool
	ovModel      string
	ovAlias      string
	ovInput      string
	ovOutput     string
	ovCacheRead  string
	ovCacheWrite string
	ovErr        string
}

type priceSummaryRow struct {
	Model     string
	MatchedAs string
	MatchKind model.MatchKind
	Source    string
	InRate    float64
	OutRate   float64
	CRRate    float64
	CWRate    float64
	Calls     int
	Tokens    int64
	CostUSD   float64
}

// NewApp constructs the Finch UI controller.
func NewApp(sc *scanner.Scanner, cat *pricing.Catalog) *App {
	return &App{
		scanner:      sc,
		catalog:      cat,
		theme:        LavenderLightTheme(),
		rangeIdx:     2, // Default to 30D
		agentOpt:     "All agents",
		modelOpt:     "All models",
		dirtySnap:    true,
		speedSortCol: "samples",
		speedSortAsc: false,
		reqSel:       -1,
		reqSort:      ui.SortOrder{Column: "time", Descending: true},
		status:       "Scanning local logs...",
	}
}

// Run launches the MyGo native desktop window.
func Run(sc *scanner.Scanner, cat *pricing.Catalog) error {
	app := NewApp(sc, cat)
	app.StartAsyncRefresh(false)

	mygo.App.WhenReady(func() {
		mygo.NewWindow(mygo.WindowOptions{
			Title:           "Finch",
			Width:           1280,
			Height:          860,
			MinWidth:        980,
			MinHeight:       640,
			BackgroundColor: "#faf8ff",
			Content:         ui.View(app.View),
		})
	})
	return mygo.App.Run()
}

// SetData populates calls directly (useful for tests or preloaded startup).
func (a *App) SetData(calls []model.Call, sum scanner.ScanSummary) {
	a.mu.Lock()
	for i := range calls {
		a.catalog.PriceCall(&calls[i])
	}
	a.allCalls = calls
	a.scanSum = sum
	a.dirtySnap = true
	a.status = fmt.Sprintf("%s calls scanned", stats.FormatInt(int64(len(calls))))
	ctx := a.uiCtx
	a.mu.Unlock()
	if ctx != nil {
		ctx.Invalidate()
	}
}

// StartAsyncRefresh scans local logs and refreshes stale (or forced) live prices in background.
func (a *App) StartAsyncRefresh(forcePrices bool) {
	a.mu.Lock()
	if a.loading {
		a.mu.Unlock()
		return
	}
	a.loading = true
	a.status = "Scanning local logs..."
	ctx := a.uiCtx
	a.mu.Unlock()
	if ctx != nil {
		ctx.Invalidate()
	}

	go func() {
		calls, sum, err := a.scanner.Scan()
		a.mu.Lock()
		if err != nil {
			a.status = "Scan error: " + err.Error()
		} else {
			for i := range calls {
				a.catalog.PriceCall(&calls[i])
			}
			a.allCalls = calls
			a.scanSum = sum
			a.dirtySnap = true
			a.status = fmt.Sprintf("%s calls across %d files", stats.FormatInt(int64(sum.CallsTotal)), sum.FilesTotal)
		}
		a.loading = false
		c := a.uiCtx
		a.mu.Unlock()
		if c != nil {
			c.Invalidate()
		}

		if forcePrices || a.catalog.IsStale() {
			a.RefreshPricesAsync()
		}
	}()
}

// RefreshPricesAsync fetches live models.dev + OpenRouter pricing in background and re-prices all calls.
func (a *App) RefreshPricesAsync() {
	a.mu.Lock()
	if a.syncing {
		a.mu.Unlock()
		return
	}
	a.syncing = true
	a.status = "Syncing live prices..."
	c := a.uiCtx
	a.mu.Unlock()
	if c != nil {
		c.Invalidate()
	}

	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
		defer cancel()
		err := a.catalog.Refresh(ctx)

		a.mu.Lock()
		a.syncing = false
		for i := range a.allCalls {
			a.catalog.PriceCall(&a.allCalls[i])
		}
		a.dirtySnap = true
		cnt, _, _ := a.catalog.Status()
		if err != nil {
			a.status = fmt.Sprintf("Price sync failed (%d cached)", cnt)
		} else {
			a.status = fmt.Sprintf("Synced %s live prices", stats.FormatInt(int64(cnt)))
		}
		uiCtx := a.uiCtx
		a.mu.Unlock()
		if uiCtx != nil {
			uiCtx.Invalidate()
		}
	}()
}

func (a *App) ensureSnapshot() {
	a.mu.Lock()
	defer a.mu.Unlock()
	if !a.dirtySnap {
		return
	}
	a.recomputeLocked()
}

func (a *App) recomputeLocked() {
	var agentFilter model.Agent
	if a.agentOpt != "" && a.agentOpt != "All agents" {
		for _, ag := range model.AllAgents {
			if model.AgentLabel(ag) == a.agentOpt {
				agentFilter = ag
				break
			}
		}
	}
	modelFilter := ""
	if a.modelOpt != "" && a.modelOpt != "All models" {
		modelFilter = a.modelOpt
	}
	f := stats.Filter{
		Range: stats.RangeKind(a.rangeIdx),
		Agent: agentFilter,
		Model: modelFilter,
		Query: a.query,
	}
	a.snap = stats.Compute(a.allCalls, f, stats.MetricKind(a.metricIdx), stats.SplitKind(a.splitIdx))
	a.sortedReqs = append([]model.Call(nil), a.snap.Filtered...)
	a.sortRequestsLocked()
	a.sortedSpeeds = append([]stats.ModelSpeed(nil), a.snap.Speeds...)
	a.sortSpeedsLocked()
	a.priceRows = a.buildPriceSummaryLocked()
	a.dirtySnap = false
}

func (a *App) sortSpeedsLocked() {
	col := a.speedSortCol
	asc := a.speedSortAsc
	sort.SliceStable(a.sortedSpeeds, func(i, j int) bool {
		si, sj := a.sortedSpeeds[i], a.sortedSpeeds[j]
		var c int
		switch col {
		case "model":
			c = strings.Compare(si.Model, sj.Model)
		case "p50tps":
			c = cmpFloat64(si.MedianTPS, sj.MedianTPS)
		case "wtps":
			c = cmpFloat64(si.WeightedTPS, sj.WeightedTPS)
		case "ttft":
			c = cmpFloat64(si.MedianTTFTMs, sj.MedianTTFTMs)
		case "p95ttft":
			c = cmpFloat64(si.P95TTFTMs, sj.P95TTFTMs)
		case "dur":
			c = cmpFloat64(si.MedianDurMs, sj.MedianDurMs)
		default: // samples
			c = cmpInt64(int64(si.Samples), int64(sj.Samples))
		}
		if c == 0 {
			return si.Model < sj.Model
		}
		if asc {
			return c < 0
		}
		return c > 0
	})
}

func (a *App) sortRequestsLocked() {
	col := a.reqSort.Column
	desc := a.reqSort.Descending
	cmpCalls := func(x, y model.Call) int {
		switch col {
		case "agent":
			return strings.Compare(string(x.Agent), string(y.Agent))
		case "model":
			return strings.Compare(x.Model, y.Model)
		case "project":
			return strings.Compare(x.Project, y.Project)
		case "in":
			return cmpInt64(x.Tokens.Input, y.Tokens.Input)
		case "out":
			return cmpInt64(x.Tokens.Output, y.Tokens.Output)
		case "cache":
			return cmpInt64(x.Tokens.CacheRead+x.Tokens.CacheWrite, y.Tokens.CacheRead+y.Tokens.CacheWrite)
		case "ttft":
			return cmpInt64(x.TTFTMs, y.TTFTMs)
		case "tps":
			return cmpFloat64(x.TPS(), y.TPS())
		case "cost":
			return cmpFloat64(x.CostUSD, y.CostUSD)
		default:
			if x.Timestamp.Before(y.Timestamp) {
				return -1
			}
			if x.Timestamp.After(y.Timestamp) {
				return 1
			}
			return strings.Compare(x.ID, y.ID)
		}
	}
	sort.SliceStable(a.sortedReqs, func(i, j int) bool {
		c := cmpCalls(a.sortedReqs[i], a.sortedReqs[j])
		if desc {
			return c > 0
		}
		return c < 0
	})
}

func cmpInt64(a, b int64) int {
	if a < b {
		return -1
	}
	if a > b {
		return 1
	}
	return 0
}

func cmpFloat64(a, b float64) int {
	if a < b {
		return -1
	}
	if a > b {
		return 1
	}
	return 0
}

func (a *App) buildPriceSummaryLocked() []priceSummaryRow {
	agg := make(map[string]*priceSummaryRow)
	for _, c := range a.allCalls {
		m := c.Model
		if m == "" {
			m = "(unknown)"
		}
		r := agg[m]
		if r == nil {
			res := a.catalog.Resolve(m)
			r = &priceSummaryRow{
				Model:     m,
				MatchedAs: res.MatchedAs,
				MatchKind: res.Kind,
				Source:    res.Price.Source,
				InRate:    res.Price.Input,
				OutRate:   res.Price.Output,
				CRRate:    res.Price.CacheRead,
				CWRate:    res.Price.CacheWrite,
			}
			if res.Kind == model.MatchNone && c.ReportedCost > 0 {
				r.MatchKind = model.MatchReported
				r.Source = "log"
			}
			agg[m] = r
		}
		r.Calls++
		r.Tokens += c.Tokens.Total()
		r.CostUSD += c.CostUSD
	}
	rows := make([]priceSummaryRow, 0, len(agg))
	for _, r := range agg {
		rows = append(rows, *r)
	}
	sort.Slice(rows, func(i, j int) bool {
		ui, uj := rows[i].MatchKind == model.MatchNone, rows[j].MatchKind == model.MatchNone
		if ui != uj {
			return ui
		}
		if rows[i].CostUSD != rows[j].CostUSD {
			return rows[i].CostUSD > rows[j].CostUSD
		}
		return rows[i].Calls > rows[j].Calls
	})
	return rows
}

// View renders the full Finch UI frame.
func (a *App) View(c *ui.Context) {
	a.mu.Lock()
	a.uiCtx = c
	a.mu.Unlock()

	c.SetTheme(a.theme)
	a.ensureSnapshot()

	ui.Column(c).Fill().Background(ColBg).Children(func() {
		a.renderHeader(c)
		a.renderFilterBar(c)

		switch a.tab {
		case tabOverview:
			ui.Scroll(c).Grow(1).FillWidth().Children(func() {
				ui.Column(c).FillWidth().Padding(20, 28, 32, 28).Gap(28).Children(func() {
					a.renderKPIStrip(c)
					a.renderTrendAndBreakdown(c)
					a.renderSpeedTable(c)
					a.renderHeatmapSection(c)
				})
			})
		case tabRequests:
			a.renderRequestsTab(c)
		case tabPricing:
			a.renderPricingTab(c)
		}
	})

	a.renderOverrideModal(c)
}

// renderHeader renders a single calm architectural header bar:
// strong brand wordmark, quiet underline navigation, range toggle, and one contextual action.
func (a *App) renderHeader(c *ui.Context) {
	a.mu.Lock()
	loading := a.loading
	syncing := a.syncing
	a.mu.Unlock()

	ui.Row(c).FillWidth().Padding(0, 28).Height(52).Gap(20).AlignItems(ui.Center).
		Background(ColBg).BorderWidth(0, 0, 1, 0).BorderColor(ColHairline).Children(func() {
		// Brand wordmark (noticeably stronger weight/scale than nav items, no decorative dots/pills)
		ui.Text(c, "Finch").FontSize(19).Bold().TextColor(ColInk).NoWrap()

		// Underline navigation tabs
		ui.Row(c).Gap(4).FillHeight().AlignItems(ui.Stretch).Children(func() {
			navTab(c, "Overview", a.tab == tabOverview, func() {
				a.tab = tabOverview
				a.markDirty()
			})
			navTab(c, "Requests", a.tab == tabRequests, func() {
				a.tab = tabRequests
				a.markDirty()
			})
			navTab(c, "Pricing", a.tab == tabPricing, func() {
				a.tab = tabPricing
				a.markDirty()
			})
		})

		ui.Spacer(c).Grow(1)

		// Time range selector (only relevant to Overview & Requests)
		if a.tab != tabPricing {
			if ui.Segmented(c, &a.rangeIdx, "Today", "7D", "30D", "All").Label("Time range").Changed() {
				a.markDirty()
			}
		}

		// Quiet refresh text link instead of button wall
		refreshLbl := "Refresh"
		if loading || syncing {
			refreshLbl = "Updating..."
		}
		ref := ui.Box(c).Padding(5, 10).Radius(6).Cursor(ui.CursorPointer).
			Tooltip("Re-scan local logs and refresh stale pricing").Children(func() {
			ui.Text(c, refreshLbl).FontSize(13).TextColor(ColInkMuted).NoWrap()
		})
		if ref.Hovered() {
			ref.Background(ColSurfaceSubtle)
		}
		if ref.Clicked() {
			a.StartAsyncRefresh(false)
		}

		// Single contextual primary CTA per section
		switch a.tab {
		case tabRequests:
			if ui.PrimaryButton(c, "Export CSV").Clicked() {
				a.exportCSV()
			}
		case tabPricing:
			lbl := "Sync prices"
			if syncing {
				lbl = "Syncing..."
			}
			if ui.PrimaryButton(c, lbl).Clicked() {
				a.RefreshPricesAsync()
			}
		}
	})
}

func navTab(c *ui.Context, label string, active bool, onSelect func()) {
	col := ColInkMuted
	borderCol := ui.RGBA(0, 0, 0, 0)
	if active {
		col = ColInk
		borderCol = ColAccent
	}
	el := ui.Row(c).FillHeight().Padding(0, 12).AlignItems(ui.Center).
		BorderWidth(0, 0, 2, 0).BorderColor(borderCol).Cursor(ui.CursorPointer).Children(func() {
		t := ui.Text(c, label).FontSize(13).TextColor(col).NoWrap()
		if active {
			t.Bold()
		}
	})
	if !active && el.Hovered() {
		el.Background(ColSurfaceSubtle)
	}
	if el.Clicked() {
		onSelect()
	}
}

func (a *App) renderFilterBar(c *ui.Context) {
	if a.tab == tabPricing {
		return
	}
	a.mu.Lock()
	statusText := a.status
	allAgents := a.snap.AllAgents
	allModels := a.snap.AllModels
	unpriced := len(a.snap.KPI.UnpricedModels)
	a.mu.Unlock()

	agentOpts := make([]string, 0, len(allAgents)+1)
	agentOpts = append(agentOpts, "All agents")
	for _, ag := range allAgents {
		agentOpts = append(agentOpts, model.AgentLabel(ag))
	}
	modelOpts := make([]string, 0, len(allModels)+1)
	modelOpts = append(modelOpts, "All models")
	modelOpts = append(modelOpts, allModels...)

	ui.Row(c).FillWidth().Padding(10, 28).Gap(10).AlignItems(ui.Center).
		Background(ColBg).BorderWidth(0, 0, 1, 0).BorderColor(ColHairline).Children(func() {
		if ui.Select(c, &a.agentOpt, agentOpts).Width(140).Changed() {
			a.markDirty()
		}
		if ui.Select(c, &a.modelOpt, modelOpts).Width(230).Changed() {
			a.markDirty()
		}
		if ui.SearchField(c, &a.query).Placeholder("Filter model, project, or session").Width(240).Changed() {
			a.markDirty()
		}
		if a.agentOpt != "All agents" || a.modelOpt != "All models" || a.query != "" {
			rst := ui.Box(c).Padding(5, 8).Radius(6).Cursor(ui.CursorPointer).Children(func() {
				ui.Text(c, "Clear").FontSize(13).TextColor(ColAccent).NoWrap()
			})
			if rst.Hovered() {
				rst.Background(ColAccentSoft)
			}
			if rst.Clicked() {
				a.agentOpt = "All agents"
				a.modelOpt = "All models"
				a.query = ""
				a.markDirty()
			}
		}

		ui.Spacer(c).Grow(1)

		// Quiet plain-text telemetry (no floating badge chips)
		if unpriced > 0 {
			lnk := ui.Box(c).Padding(3, 6).Radius(5).Cursor(ui.CursorPointer).
				Tooltip("Open Pricing to map unpriced models").Children(func() {
				ui.Text(c, fmt.Sprintf("%d unpriced", unpriced)).FontSize(12).TextColor(ColWarning).NoWrap()
			})
			if lnk.Hovered() {
				lnk.Background(ColSurfaceSubtle)
			}
			if lnk.Clicked() {
				a.tab = tabPricing
			}
			ui.Text(c, "·").FontSize(12).TextColor(ColInkSubtle)
		}
		if statusText != "" {
			ui.Text(c, statusText).FontSize(12).TextColor(ColInkSubtle).NoWrap()
		}
	})
}

// renderKPIStrip draws a frameless 5-column architectural stat band separated by
// hairline rules instead of 5 boxy cards with colored dots.
func (a *App) renderKPIStrip(c *ui.Context) {
	k := a.snap.KPI

	cachePct := "—"
	if k.InputTokens+k.CacheRead+k.CacheWrite > 0 {
		cachePct = fmt.Sprintf("%.1f%%", k.CacheHitRatio*100)
	}
	tpsVal := stats.FormatTPS(k.MedianTPS)
	tpsSub := "No timed calls"
	if k.SpeedSamples > 0 {
		tpsSub = fmt.Sprintf("Weighted %s · %s timed", stats.FormatTPS(k.WeightedTPS), stats.FormatInt(int64(k.SpeedSamples)))
	}
	latSub := "No TTFT samples"
	if k.TTFTSamples > 0 {
		latSub = fmt.Sprintf("p95 %s · dur %s", stats.FormatMs(k.P95TTFTMs), stats.FormatMs(k.MedianDurMs))
	} else if k.MedianDurMs > 0 {
		latSub = fmt.Sprintf("Median duration %s", stats.FormatMs(k.MedianDurMs))
	}

	ui.Row(c).FillWidth().Padding(4, 0, 20, 0).BorderWidth(0, 0, 1, 0).BorderColor(ColHairline).Children(func() {
		kpiCell(c, "Estimated spend", stats.FormatUSD(k.TotalCostUSD),
			fmt.Sprintf("%s reqs · %s sessions", stats.FormatInt(int64(k.Requests)), stats.FormatInt(int64(k.Sessions))), false)
		kpiCell(c, "Total tokens", stats.FormatTokens(k.TotalTokens),
			fmt.Sprintf("%s in · %s out", stats.FormatTokens(k.InputTokens), stats.FormatTokens(k.OutputTokens)), true)
		kpiCell(c, "Cache hit rate", cachePct,
			fmt.Sprintf("%s read · %s write", stats.FormatTokens(k.CacheRead), stats.FormatTokens(k.CacheWrite)), true)
		kpiCell(c, "Median speed", tpsVal, tpsSub, true)
		kpiCell(c, "First token (p50)", stats.FormatMs(k.MedianTTFTMs), latSub, true)
	})
}

func kpiCell(c *ui.Context, label, value, sub string, leftDivider bool) {
	col := ui.Column(c).Grow(1).MinWidth(0).Gap(5)
	if leftDivider {
		col.Padding(0, 16, 0, 20).BorderWidth(0, 0, 0, 1).BorderColor(ColHairline)
	} else {
		col.Padding(0, 16, 0, 0)
	}
	col.Children(func() {
		ui.Text(c, label).FontSize(13).TextColor(ColInkMuted).NoWrap()
		ui.Text(c, value).FontSize(28).Bold().TextColor(ColInk).NoWrap()
		ui.Text(c, sub).FontSize(12).TextColor(ColInkSubtle).NoWrap()
	})
}

func (a *App) renderTrendAndBreakdown(c *ui.Context) {
	ui.Row(c).FillWidth().Gap(28).AlignItems(ui.Start).Children(func() {
		// Left (65%): Usage trend plot
		ui.Column(c).Grow(19).MinWidth(0).Gap(14).Children(func() {
			ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
				ui.Text(c, "Usage trend").FontSize(15).Bold().TextColor(ColInk)
				ui.Spacer(c).Grow(1)
				if ui.Segmented(c, &a.metricIdx, "Cost", "Tokens", "Reqs").Label("Metric").Changed() {
					a.markDirty()
				}
				if ui.Segmented(c, &a.splitIdx, "Agent", "Model").Label("Split").Changed() {
					a.markDirty()
				}
			})

			ui.Box(c).FillWidth().Padding(16, 16, 12, 12).Radius(10).
				Background(ColSurface).Border(1, ColHairline).Children(func() {
				a.renderStackedBarChart(c)
			})

			// Calm inline legend
			if len(a.snap.SeriesNames) > 0 {
				ui.Row(c).FillWidth().Gap(16).Wrap().Children(func() {
					for i, name := range a.snap.SeriesNames {
						lbl := name
						if stats.SplitKind(a.splitIdx) == stats.SplitAgent && name != "Other" {
							lbl = model.AgentLabel(model.Agent(name))
						}
						col := seriesColor(i)
						if name == "Other" {
							col = SeriesPalette[len(SeriesPalette)-1]
						}
						ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
							ui.Box(c).Size(8, 8).Radius(2).Background(col)
							ui.Text(c, lbl).FontSize(12).TextColor(ColInkMuted).NoWrap()
						})
					}
				})
			}
		})

		// Right (35%): Editorial breakdown list (no card cage, hairline-separated rows)
		ui.Column(c).Grow(10).MinWidth(290).Gap(12).Children(func() {
			heading := "Breakdown by agent"
			if stats.SplitKind(a.splitIdx) == stats.SplitModel {
				heading = "Breakdown by model"
			}
			ui.Row(c).FillWidth().AlignItems(ui.Center).Children(func() {
				ui.Text(c, heading).FontSize(15).Bold().TextColor(ColInk)
				ui.Spacer(c).Grow(1)
				ui.Text(c, fmt.Sprintf("%d", len(a.snap.Rankings))).FontSize(12).TextColor(ColInkSubtle)
			})

			if len(a.snap.Rankings) == 0 {
				ui.Text(c, "No activity in selected range.").FontSize(13).TextColor(ColInkMuted).Padding(12, 0)
				return
			}

			maxRows := min(len(a.snap.Rankings), 7)
			ui.Column(c).FillWidth().Children(func() {
				for i := range maxRows {
					r := a.snap.Rankings[i]
					col := seriesColor(i)
					if i >= 5 {
						col = SeriesPalette[len(SeriesPalette)-1]
					}
					row := ui.Column(c).FillWidth().Padding(8, 8).Gap(5).Radius(7).Cursor(ui.CursorPointer).
						BorderWidth(0, 0, 1, 0).BorderColor(ColHairline).
						Tooltip("Click to filter by " + r.Label).Children(func() {
						ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
							ui.Box(c).Size(8, 8).Radius(2).Background(col)
							ui.Text(c, r.Label).FontSize(13).Bold().TextColor(ColInk).SingleLine().Grow(1).MinWidth(0)
							ui.Text(c, formatRankVal(r, stats.MetricKind(a.metricIdx))).FontSize(13).Bold().TextColor(ColInk).NoWrap()
						})
						ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
							// Whisper-thin 2px proportion hairline
							ui.Box(c).Grow(1).Height(2).Radius(1).Background(ColSurfaceSubtle).Children(func() {
								pct := float32(r.Share * 100)
								if pct < 1 && r.Share > 0 {
									pct = 1
								}
								ui.Box(c).WidthPercent(pct).Height(2).Radius(1).Background(col)
							})
							pctStr := fmt.Sprintf("%.0f%%", r.Share*100)
							sub := fmt.Sprintf("%s · %s", stats.FormatTokens(r.Tokens), pctStr)
							if stats.MetricKind(a.metricIdx) == stats.MetricTokens {
								sub = fmt.Sprintf("%s · %s", stats.FormatUSD(r.CostUSD), pctStr)
							}
							ui.Text(c, sub).FontSize(12).TextColor(ColInkSubtle).NoWrap()
						})
					})
					if row.Hovered() {
						row.Background(ColSurfaceSubtle)
					}
					if row.Clicked() {
						if stats.SplitKind(a.splitIdx) == stats.SplitAgent {
							a.agentOpt = r.Label
						} else {
							a.modelOpt = r.Key
						}
						a.markDirty()
					}
				}
			})
		})
	})
}

func (a *App) renderStackedBarChart(c *ui.Context) {
	buckets := a.snap.Buckets
	if len(buckets) == 0 {
		ui.Box(c).FillWidth().Height(220).Center().Children(func() {
			ui.Text(c, "No activity in this range").FontSize(13).TextColor(ColInkMuted)
		})
		return
	}

	labels := make([]string, len(buckets))
	maxY := 0.0
	for i, b := range buckets {
		labels[i] = b.Label
		if b.Total > maxY {
			maxY = b.Total
		}
	}
	if maxY == 0 {
		maxY = 1
	}

	x := chart.NewBand(labels...).Padding(0.28, 0.06)
	y := chart.NewLinear(0, maxY).Nice(4)
	metric := stats.MetricKind(a.metricIdx)
	y.Format = func(v float64) string {
		if v == 0 {
			if metric == stats.MetricCost {
				return "$0"
			}
			return "0"
		}
		switch metric {
		case stats.MetricCost:
			if v >= 10 {
				return "$" + stats.FormatInt(int64(v+0.5))
			}
			return stats.FormatUSD(v)
		case stats.MetricTokens:
			return stats.FormatTokens(int64(v))
		default:
			return stats.FormatInt(int64(v + 0.5))
		}
	}
	yTicks := y.Ticks(4)

	allXTicks := x.Ticks()
	step := max(1, len(allXTicks)/7)
	var xTicks []chart.Tick
	lastIdx := -step
	for i, tk := range allXTicks {
		if i%step == 0 {
			// Avoid placing a tick right next to the final label.
			if len(allXTicks)-1-i < max(2, step/2) && i != len(allXTicks)-1 {
				continue
			}
			xTicks = append(xTicks, tk)
			lastIdx = i
		}
	}
	if len(allXTicks) > 0 && lastIdx != len(allXTicks)-1 {
		xTicks = append(xTicks, allXTicks[len(allXTicks)-1])
	}

	axes := chart.Axes{
		Left:   yTicks,
		Bottom: xTicks,
		Label: func(_ chart.Side, tk chart.Tick) {
			ui.Text(c, tk.Label).NoWrap().FontSize(12).TextColor(ColInkSubtle)
		},
	}

	chart.Plot(c, axes, func(area ui.Element) {
		area.Draw(func(p *ui.Painter, r ui.Rect) {
			for _, tk := range yTicks {
				p.Line(r.X, chart.Y(r, tk.At), r.X+r.W, chart.Y(r, tk.At), 1, ColHairline)
			}
		})
		for bi, b := range buckets {
			if b.Total <= 0 {
				continue
			}
			base := 0.0
			for si, sName := range a.snap.SeriesNames {
				val := b.Values[sName]
				if val <= 0 {
					continue
				}
				col := seriesColor(si)
				if sName == "Other" {
					col = SeriesPalette[len(SeriesPalette)-1]
				}
				lbl := sName
				if stats.SplitKind(a.splitIdx) == stats.SplitAgent && sName != "Other" {
					lbl = model.AgentLabel(model.Agent(sName))
				}
				tip := fmt.Sprintf("%s · %s: %s (Total %s)", b.Key, lbl, formatMetricVal(val, metric), formatMetricVal(b.Total, metric))
				chart.Rect(c, x.Band(bi), y.Span(base, base+val)).
					Background(col).Radius(2).Tooltip(tip)
				base += val
			}
		}
	}).Label("Usage trend").FillWidth().Height(220).Gap(8)
}

// renderSpeedTable renders a crisp, sortable typographic telemetry table (inspired by
// Magpie's Model speed section — zero noisy progress bars, pure scannable numbers).
func (a *App) renderSpeedTable(c *ui.Context) {
	speeds := a.sortedSpeeds

	ui.Column(c).FillWidth().Padding(20, 0, 0, 0).Gap(12).
		BorderWidth(1, 0, 0, 0).BorderColor(ColHairline).Children(func() {
		ui.Row(c).FillWidth().AlignItems(ui.Center).Children(func() {
			ui.Text(c, "Model speed and latency").FontSize(15).Bold().TextColor(ColInk)
			ui.Spacer(c).Grow(1)
			ui.Text(c, "Click any column to sort · click a model to filter").FontSize(12).TextColor(ColInkSubtle)
		})

		if len(speeds) == 0 {
			ui.Text(c, "No timed requests in the selected range.").FontSize(13).TextColor(ColInkMuted).Padding(8, 0)
			return
		}

		ui.Column(c).FillWidth().Radius(10).Background(ColSurface).Border(1, ColHairline).Children(func() {
			// Sortable header row
			ui.Row(c).FillWidth().Padding(10, 16).Gap(12).AlignItems(ui.Center).
				BorderWidth(0, 0, 1, 0).BorderColor(ColHairline).Children(func() {
				a.speedColHead(c, "Model", "model", 0, false)
				a.speedColHead(c, "Median speed", "p50tps", 125, true)
				a.speedColHead(c, "Weighted", "wtps", 115, true)
				a.speedColHead(c, "First token", "ttft", 110, true)
				a.speedColHead(c, "p95 TTFT", "p95ttft", 105, true)
				a.speedColHead(c, "Duration", "dur", 105, true)
				a.speedColHead(c, "Requests", "samples", 90, true)
			})

			limit := min(len(speeds), 10)
			for i := range limit {
				ms := speeds[i]
				borderW := float32(1)
				if i == limit-1 {
					borderW = 0
				}
				row := ui.Row(c).FillWidth().Padding(9, 16).Gap(12).AlignItems(ui.Center).
					BorderWidth(0, 0, borderW, 0).BorderColor(ColHairline).
					Cursor(ui.CursorPointer).Tooltip("Filter dashboard by " + ms.Model).Children(func() {
					ui.Text(c, ms.Model).FontSize(13).Bold().TextColor(ColInk).SingleLine().Grow(1).MinWidth(0)
					rightCell(c, stats.FormatTPS(ms.MedianTPS), 125, true, ColInk)
					rightCell(c, stats.FormatTPS(ms.WeightedTPS), 115, false, ColInkMuted)
					rightCell(c, stats.FormatMs(ms.MedianTTFTMs), 110, false, ColInk)
					rightCell(c, stats.FormatMs(ms.P95TTFTMs), 105, false, ColInkMuted)
					rightCell(c, stats.FormatMs(ms.MedianDurMs), 105, false, ColInkMuted)
					rightCell(c, stats.FormatInt(int64(ms.Samples)), 90, false, ColInkSubtle)
				})
				if row.Hovered() {
					row.Background(ColSurfaceSubtle)
				}
				if row.Clicked() {
					a.modelOpt = ms.Model
					a.markDirty()
				}
			}
		})
	})
}

func (a *App) speedColHead(c *ui.Context, label, key string, width float32, alignRight bool) {
	active := a.speedSortCol == key
	text := label
	if active {
		if a.speedSortAsc {
			text += " ↑"
		} else {
			text += " ↓"
		}
	}
	col := ColInkMuted
	if active {
		col = ColInk
	}
	box := ui.Row(c).Cursor(ui.CursorPointer).AlignItems(ui.Center)
	if width > 0 {
		box.Width(width)
	} else {
		box.Grow(1).MinWidth(0)
	}
	if alignRight {
		box.Justify(ui.End)
	}
	box.Children(func() {
		t := ui.Text(c, text).FontSize(12).TextColor(col).NoWrap()
		if active {
			t.Bold()
		}
	})
	if box.Clicked() {
		if a.speedSortCol == key {
			a.speedSortAsc = !a.speedSortAsc
		} else {
			a.speedSortCol = key
			a.speedSortAsc = (key == "model" || key == "ttft" || key == "p95ttft" || key == "dur")
		}
		a.mu.Lock()
		a.sortSpeedsLocked()
		a.mu.Unlock()
	}
}

func rightCell(c *ui.Context, text string, width float32, bold bool, color ui.Color) {
	ui.Row(c).Width(width).Justify(ui.End).Children(func() {
		t := ui.Text(c, text).FontSize(13).TextColor(color).NoWrap()
		if bold {
			t.Bold()
		}
	})
}

func (a *App) renderHeatmapSection(c *ui.Context) {
	days := a.snap.Heatmap
	metric := stats.MetricKind(a.metricIdx)

	ui.Column(c).FillWidth().Padding(20, 0, 0, 0).Gap(12).
		BorderWidth(1, 0, 0, 0).BorderColor(ColHairline).Children(func() {
		ui.Row(c).FillWidth().AlignItems(ui.Center).Gap(10).Children(func() {
			ui.Text(c, "Activity heatmap").FontSize(15).Bold().TextColor(ColInk)
			ui.Text(c, "Last 53 weeks").FontSize(12).TextColor(ColInkSubtle)
			ui.Spacer(c).Grow(1)
			ui.Row(c).Gap(4).AlignItems(ui.Center).Children(func() {
				ui.Text(c, "Less").FontSize(12).TextColor(ColInkSubtle)
				for _, col := range HeatmapRamp {
					ui.Box(c).Size(10, 10).Radius(2).Background(col)
				}
				ui.Text(c, "More").FontSize(12).TextColor(ColInkSubtle)
			})
		})

		if len(days) != 53*7 {
			return
		}

		maxVal := 0.0
		for _, d := range days {
			if v := heatVal(d, metric); v > maxVal {
				maxVal = v
			}
		}

		weekLabels := make([]string, 53)
		for w := range 53 {
			weekLabels[w] = strconv.Itoa(w)
		}
		x := chart.NewBand(weekLabels...).Padding(0.24, 0.01)
		y := chart.NewBand("Sat", "Fri", "Thu", "Wed", "Tue", "Mon", "Sun").Padding(0.24, 0.03)

		var topTicks []chart.Tick
		lastMonth := time.Month(0)
		for w := range 53 {
			m := days[w*7].Date.Month()
			if m != lastMonth {
				lastMonth = m
				topTicks = append(topTicks, chart.Tick{At: x.At(w), Label: days[w*7].Date.Format("Jan")})
			}
		}
		leftTicks := []chart.Tick{
			{At: y.At(5), Label: "Mon"},
			{At: y.At(3), Label: "Wed"},
			{At: y.At(1), Label: "Fri"},
		}

		axes := chart.Axes{
			Top:  topTicks,
			Left: leftTicks,
			Label: func(_ chart.Side, tk chart.Tick) {
				ui.Text(c, tk.Label).NoWrap().FontSize(12).TextColor(ColInkSubtle)
			},
		}

		ui.Box(c).FillWidth().Padding(14, 16).Radius(10).Background(ColSurface).Border(1, ColHairline).Children(func() {
			chart.Plot(c, axes, func(_ ui.Element) {
				for i, d := range days {
					w := i / 7
					dow := i % 7
					rowIdx := 6 - dow
					v := heatVal(d, metric)
					lvl := heatLevel(v, maxVal)
					tip := fmt.Sprintf("%s · %s · %s tok · %s reqs",
						d.DateKey, stats.FormatUSD(d.CostUSD), stats.FormatTokens(d.Tokens), stats.FormatInt(int64(d.Requests)))
					chart.Rect(c, x.Band(w), y.Band(rowIdx)).
						Background(HeatmapRamp[lvl]).Radius(2).Tooltip(tip)
				}
			}).Label("53-week activity heatmap").FillWidth().Height(126).Gap(6)
		})
	})
}

func (a *App) renderRequestsTab(c *ui.Context) {
	cols := []ui.TableColumn{
		{ID: "time", Title: "Time", Width: 138, Sortable: true},
		{ID: "agent", Title: "Agent", Width: 96, Sortable: true},
		{ID: "model", Title: "Model", Width: 230, Sortable: true},
		{ID: "project", Title: "Project", Width: 130, Sortable: true},
		{ID: "in", Title: "Input", Width: 78, Align: ui.End, Sortable: true},
		{ID: "out", Title: "Output", Width: 78, Align: ui.End, Sortable: true},
		{ID: "cache", Title: "Cache R/W", Width: 114, Align: ui.End, Sortable: true},
		{ID: "ttft", Title: "TTFT", Width: 82, Align: ui.End, Sortable: true},
		{ID: "tps", Title: "Speed", Width: 92, Align: ui.End, Sortable: true},
		{ID: "cost", Title: "Cost", Width: 86, Align: ui.End, Sortable: true},
		{ID: "match", Title: "Pricing", Width: 84},
	}

	prevSort := a.reqSort
	a.reqTable.Selected = &a.reqSel
	a.reqTable.Sort = &a.reqSort
	a.reqTable.RowHeight = 32
	a.reqTable.RoundRows = true

	ui.Column(c).Grow(1).FillWidth().Padding(16, 28, 20, 28).Gap(10).Children(func() {
		ui.Row(c).FillWidth().AlignItems(ui.Center).Children(func() {
			ui.Text(c, fmt.Sprintf("%s requests", stats.FormatInt(int64(len(a.sortedReqs))))).FontSize(13).Bold().TextColor(ColInk)
			ui.Spacer(c).Grow(1)
			ui.Text(c, "Click any column header to sort").FontSize(12).TextColor(ColInkSubtle)
		})

		ui.Box(c).Grow(1).FillWidth().Radius(10).Background(ColSurface).
			Border(1, ColHairline).Padding(4).Children(func() {
			ui.Table(c, &a.reqTable, cols, len(a.sortedReqs), func(row, col int) {
				req := a.sortedReqs[row]
				switch col {
				case 0:
					ui.Text(c, req.Timestamp.Local().Format("01-02 15:04:05")).FontSize(13).TextColor(ColInkMuted).SingleLine()
				case 1:
					ui.Text(c, model.AgentLabel(req.Agent)).FontSize(13).Bold().TextColor(ColAccent).SingleLine()
				case 2:
					tip := req.Model
					if req.MatchedAs != "" && req.MatchedAs != req.Model {
						tip += " -> " + req.MatchedAs
					}
					ui.Text(c, req.Model).FontSize(13).TextColor(ColInk).SingleLine().Tooltip(tip)
				case 3:
					p := req.Project
					if p == "" {
						p = "—"
					}
					ui.Text(c, p).FontSize(13).TextColor(ColInkMuted).SingleLine()
				case 4:
					ui.Text(c, stats.FormatTokens(req.Tokens.Input)).FontSize(13).TextColor(ColInk).SingleLine()
				case 5:
					ui.Text(c, stats.FormatTokens(req.Tokens.Output)).FontSize(13).TextColor(ColInk).SingleLine()
				case 6:
					ui.Text(c, stats.FormatTokens(req.Tokens.CacheRead)+" / "+stats.FormatTokens(req.Tokens.CacheWrite)).
						FontSize(12).TextColor(ColInkMuted).SingleLine()
				case 7:
					ui.Text(c, stats.FormatMs(float64(req.TTFTMs))).FontSize(13).TextColor(ColInkMuted).SingleLine()
				case 8:
					ui.Text(c, stats.FormatTPS(req.TPS())).FontSize(13).TextColor(ColInk).SingleLine()
				case 9:
					ui.Text(c, stats.FormatUSD(req.CostUSD)).FontSize(13).Bold().TextColor(ColInk).SingleLine()
				case 10:
					matchLabel(c, req.MatchKind)
				}
			}).Grow(1).FillWidth()
		})
	})

	if a.reqSort != prevSort {
		a.mu.Lock()
		a.sortRequestsLocked()
		a.mu.Unlock()
	}
}

func (a *App) renderPricingTab(c *ui.Context) {
	cnt, updatedAt, lastErr := a.catalog.Status()
	cachePath, ovPath := pricing.DefaultPaths()

	ui.Column(c).Grow(1).FillWidth().Padding(20, 28, 24, 28).Gap(16).Children(func() {
		ui.Column(c).FillWidth().Padding(0, 0, 14, 0).Gap(4).
			BorderWidth(0, 0, 1, 0).BorderColor(ColHairline).Children(func() {
			ui.Text(c, "Live pricing catalog and suffix rules").FontSize(15).Bold().TextColor(ColInk)
			info := fmt.Sprintf("Synced from models.dev and OpenRouter · %s models cached", stats.FormatInt(int64(cnt)))
			if !updatedAt.IsZero() {
				info += " · updated " + updatedAt.Local().Format("2006-01-02 15:04")
			}
			ui.Text(c, info).FontSize(13).TextColor(ColInkMuted)
			ui.Text(c, fmt.Sprintf("Cache: %s  ·  Overrides: %s", cachePath, ovPath)).FontSize(12).TextColor(ColInkSubtle)
			if lastErr != "" {
				ui.Text(c, "Last sync warning: "+lastErr).FontSize(12).TextColor(ColWarning)
			}
		})

		cols := []ui.TableColumn{
			{ID: "model", Title: "Observed model", Width: 240},
			{ID: "match", Title: "Match", Width: 86},
			{ID: "resolved", Title: "Resolved catalog ID", Width: 215},
			{ID: "rates", Title: "In / Out / Cache R / Cache W ($/1M)", Width: 245},
			{ID: "calls", Title: "Calls", Width: 80, Align: ui.End},
			{ID: "cost", Title: "Spend", Width: 94, Align: ui.End},
			{ID: "action", Title: "Override", Width: 86},
		}
		a.priceTable.RowHeight = 36
		a.priceTable.RoundRows = true

		ui.Box(c).Grow(1).FillWidth().Radius(10).Background(ColSurface).
			Border(1, ColHairline).Padding(4).Children(func() {
			ui.Table(c, &a.priceTable, cols, len(a.priceRows), func(row, col int) {
				pr := a.priceRows[row]
				switch col {
				case 0:
					ui.Text(c, pr.Model).FontSize(13).Bold().TextColor(ColInk).SingleLine()
				case 1:
					matchLabel(c, pr.MatchKind)
				case 2:
					res := pr.MatchedAs
					if res == "" {
						res = "—"
					}
					ui.Text(c, res).FontSize(13).TextColor(ColInkMuted).SingleLine()
				case 3:
					rates := "—"
					if pr.InRate > 0 || pr.OutRate > 0 {
						rates = fmt.Sprintf("$%.2f / $%.2f / $%.2f / $%.2f", pr.InRate, pr.OutRate, pr.CRRate, pr.CWRate)
					}
					ui.Text(c, rates).FontSize(12).TextColor(ColInkMuted).SingleLine()
				case 4:
					ui.Text(c, stats.FormatInt(int64(pr.Calls))).FontSize(13).TextColor(ColInk).SingleLine()
				case 5:
					ui.Text(c, stats.FormatUSD(pr.CostUSD)).FontSize(13).Bold().TextColor(ColInk).SingleLine()
				case 6:
					btn := ui.Box(c).Padding(4, 10).Radius(6).Cursor(ui.CursorPointer).Children(func() {
						ui.Text(c, "Edit").FontSize(12).Bold().TextColor(ColAccent).NoWrap()
					})
					if btn.Hovered() {
						btn.Background(ColAccentSoft)
					}
					if btn.Clicked() {
						a.openOverrideModal(pr)
					}
				}
			}).Grow(1).FillWidth()
		})
	})
}

func (a *App) openOverrideModal(pr priceSummaryRow) {
	a.ovModel = pr.Model
	a.ovAlias = pr.MatchedAs
	if pr.InRate > 0 {
		a.ovInput = strconv.FormatFloat(pr.InRate, 'f', -1, 64)
	} else {
		a.ovInput = ""
	}
	if pr.OutRate > 0 {
		a.ovOutput = strconv.FormatFloat(pr.OutRate, 'f', -1, 64)
	} else {
		a.ovOutput = ""
	}
	if pr.CRRate > 0 {
		a.ovCacheRead = strconv.FormatFloat(pr.CRRate, 'f', -1, 64)
	} else {
		a.ovCacheRead = ""
	}
	if pr.CWRate > 0 {
		a.ovCacheWrite = strconv.FormatFloat(pr.CWRate, 'f', -1, 64)
	} else {
		a.ovCacheWrite = ""
	}
	a.ovErr = ""
	a.ovOpen = true
}

func (a *App) renderOverrideModal(c *ui.Context) {
	ui.Modal(c, &a.ovOpen, func() {
		ui.Column(c).Width(440).Padding(22).Gap(14).Radius(12).
			Background(ColSurface).Border(1, ColBorder).Shadow(0, 12, 32, 0, ui.RGBA(36, 29, 53, 0.14)).Children(func() {
			ui.Text(c, "Override price: "+a.ovModel).FontSize(15).Bold().TextColor(ColInk)
			ui.Text(c, "Map to an existing catalog model ID, or leave alias empty to enter custom $/1M token rates.").
				FontSize(13).TextColor(ColInkMuted)

			ui.Column(c).Gap(4).FillWidth().Children(func() {
				ui.Text(c, "Alias to catalog model").FontSize(12).Bold().TextColor(ColInkMuted)
				ui.TextInput(c, &a.ovAlias).Placeholder("e.g. claude-opus-5-5 or gemini-3.8-flash").FillWidth()
			})

			ui.Row(c).Gap(10).FillWidth().Children(func() {
				ui.Column(c).Gap(4).Grow(1).Children(func() {
					ui.Text(c, "Input ($/1M)").FontSize(12).Bold().TextColor(ColInkMuted)
					ui.TextInput(c, &a.ovInput).Placeholder("2.0").FillWidth()
				})
				ui.Column(c).Gap(4).Grow(1).Children(func() {
					ui.Text(c, "Output ($/1M)").FontSize(12).Bold().TextColor(ColInkMuted)
					ui.TextInput(c, &a.ovOutput).Placeholder("10.0").FillWidth()
				})
			})
			ui.Row(c).Gap(10).FillWidth().Children(func() {
				ui.Column(c).Gap(4).Grow(1).Children(func() {
					ui.Text(c, "Cache read ($/1M)").FontSize(12).Bold().TextColor(ColInkMuted)
					ui.TextInput(c, &a.ovCacheRead).Placeholder("0.2").FillWidth()
				})
				ui.Column(c).Gap(4).Grow(1).Children(func() {
					ui.Text(c, "Cache write ($/1M)").FontSize(12).Bold().TextColor(ColInkMuted)
					ui.TextInput(c, &a.ovCacheWrite).Placeholder("2.5").FillWidth()
				})
			})

			if a.ovErr != "" {
				ui.Text(c, a.ovErr).FontSize(12).TextColor(ColDanger)
			}

			ui.Row(c).FillWidth().Gap(8).Justify(ui.End).Children(func() {
				if ui.Button(c, "Cancel").Clicked() {
					a.ovOpen = false
				}
				if ui.PrimaryButton(c, "Save override").Clicked() {
					a.saveOverride()
				}
			})
		})
	})
}

func (a *App) saveOverride() {
	var entry pricing.OverrideEntry
	alias := strings.TrimSpace(a.ovAlias)
	if alias != "" {
		entry.Alias = alias
	} else {
		var err error
		if a.ovInput != "" {
			if entry.Input, err = strconv.ParseFloat(strings.TrimSpace(a.ovInput), 64); err != nil {
				a.ovErr = "Invalid input rate"
				return
			}
		}
		if a.ovOutput != "" {
			if entry.Output, err = strconv.ParseFloat(strings.TrimSpace(a.ovOutput), 64); err != nil {
				a.ovErr = "Invalid output rate"
				return
			}
		}
		if a.ovCacheRead != "" {
			if entry.CacheRead, err = strconv.ParseFloat(strings.TrimSpace(a.ovCacheRead), 64); err != nil {
				a.ovErr = "Invalid cache read rate"
				return
			}
		}
		if a.ovCacheWrite != "" {
			if entry.CacheWrite, err = strconv.ParseFloat(strings.TrimSpace(a.ovCacheWrite), 64); err != nil {
				a.ovErr = "Invalid cache write rate"
				return
			}
		}
	}
	if err := a.catalog.SetOverride(a.ovModel, entry); err != nil {
		a.ovErr = err.Error()
		return
	}
	a.mu.Lock()
	for i := range a.allCalls {
		a.catalog.PriceCall(&a.allCalls[i])
	}
	a.dirtySnap = true
	a.status = "Saved override for " + a.ovModel
	a.mu.Unlock()
	a.ovOpen = false
}

func (a *App) exportCSV() {
	home, _ := os.UserHomeDir()
	dir := filepath.Join(home, "Downloads")
	_ = os.MkdirAll(dir, 0o755)
	name := fmt.Sprintf("finch-usage-%s.csv", time.Now().Format("20060102-150405"))
	path := filepath.Join(dir, name)
	f, err := os.Create(path)
	if err != nil {
		a.mu.Lock()
		a.status = "CSV export failed: " + err.Error()
		a.mu.Unlock()
		return
	}
	a.mu.Lock()
	calls := append([]model.Call(nil), a.sortedReqs...)
	a.mu.Unlock()
	err = stats.ExportCSV(f, calls)
	_ = f.Close()
	a.mu.Lock()
	if err != nil {
		a.status = "CSV export failed: " + err.Error()
	} else {
		a.status = fmt.Sprintf("Exported %s rows to ~/Downloads/%s", stats.FormatInt(int64(len(calls))), name)
	}
	a.mu.Unlock()
}

func (a *App) markDirty() {
	a.mu.Lock()
	a.dirtySnap = true
	a.mu.Unlock()
}

// matchLabel renders a restrained semantic status cell inside data tables.
func matchLabel(c *ui.Context, kind model.MatchKind) {
	bg, fg := ColSurfaceSubtle, ColInkMuted
	switch kind {
	case model.MatchExact:
		bg, fg = ui.Hex("#e7f6ee"), ColSuccess
	case model.MatchSuffix, model.MatchPrefix:
		bg, fg = ColAccentSoft, ColAccent
	case model.MatchOverride:
		bg, fg = ui.Hex("#e7eefc"), ui.Hex("#3a5ccc")
	case model.MatchReported:
		bg, fg = ui.Hex("#f3eaf7"), ui.Hex("#93498a")
	case model.MatchNone:
		bg, fg = ui.Hex("#fdf2e3"), ColWarning
	}
	ui.Box(c).Padding(2, 7).Radius(5).Background(bg).Children(func() {
		ui.Text(c, kind.String()).FontSize(11).Bold().TextColor(fg).NoWrap()
	})
}

func formatMetricVal(v float64, m stats.MetricKind) string {
	switch m {
	case stats.MetricCost:
		return stats.FormatUSD(v)
	case stats.MetricTokens:
		return stats.FormatTokens(int64(v))
	default:
		return stats.FormatInt(int64(v+0.5)) + " reqs"
	}
}

func formatRankVal(r stats.RankItem, m stats.MetricKind) string {
	switch m {
	case stats.MetricCost:
		return stats.FormatUSD(r.CostUSD)
	case stats.MetricTokens:
		return stats.FormatTokens(r.Tokens)
	default:
		return stats.FormatInt(int64(r.Requests)) + " reqs"
	}
}

func heatVal(d stats.HeatmapDay, m stats.MetricKind) float64 {
	switch m {
	case stats.MetricCost:
		return d.CostUSD
	case stats.MetricTokens:
		return float64(d.Tokens)
	default:
		return float64(d.Requests)
	}
}

func heatLevel(v, maxVal float64) int {
	if v <= 0 || maxVal <= 0 {
		return 0
	}
	ratio := v / maxVal
	switch {
	case ratio >= 0.72:
		return 4
	case ratio >= 0.42:
		return 3
	case ratio >= 0.16:
		return 2
	default:
		return 1
	}
}

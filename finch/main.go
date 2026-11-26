package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/gui"
	"github.com/Konakonai/lilya-tools/finch/internal/model"
	"github.com/Konakonai/lilya-tools/finch/internal/pricing"
	"github.com/Konakonai/lilya-tools/finch/internal/scanner"
	"github.com/Konakonai/lilya-tools/finch/internal/stats"
)

func main() {
	cliMode := flag.Bool("summary", false, "Print usage, speed & pricing summary to stdout and exit")
	refreshPrices := flag.Bool("refresh-prices", false, "Force fetch fresh prices from models.dev & OpenRouter")
	noMagpie := flag.Bool("no-magpie", false, "Exclude historical ~/.config/magpie/usage.jsonl import")
	rangeFlag := flag.String("range", "30d", "Summary range: today, 7d, 30d, all")
	flag.Parse()

	opts := scanner.DefaultOptions()
	opts.IncludeMagpie = !*noMagpie
	sc := scanner.New(opts)

	cachePath, ovPath := pricing.DefaultPaths()
	cat := pricing.NewCatalog(cachePath, ovPath)

	if *cliMode {
		if *refreshPrices || cat.IsStale() {
			ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
			if err := cat.Refresh(ctx); err != nil {
				fmt.Fprintf(os.Stderr, "warning: price refresh failed: %v\n", err)
			}
			cancel()
		}
		calls, sum, err := sc.Scan()
		if err != nil {
			log.Fatal(err)
		}
		for i := range calls {
			cat.PriceCall(&calls[i])
		}
		rng := stats.Range30D
		switch *rangeFlag {
		case "today":
			rng = stats.RangeToday
		case "7d":
			rng = stats.Range7D
		case "all":
			rng = stats.RangeAll
		}
		snap := stats.Compute(calls, stats.Filter{Range: rng}, stats.MetricCost, stats.SplitModel)
		k := snap.KPI
		priceCnt, _, _ := cat.Status()
		fmt.Printf("Finch Summary (%s) — %d calls across %d files (%d cached, %d magpie deduped), %d live prices\n",
			rng.Label(), sum.CallsTotal, sum.FilesTotal, sum.FilesCached, sum.MagpieDedup, priceCnt)
		fmt.Printf("Cost: %s | Tokens: %s (In %s, Out %s, CacheR %s, CacheW %s, Hit %.1f%%)\n",
			stats.FormatUSD(k.TotalCostUSD), stats.FormatTokens(k.TotalTokens),
			stats.FormatTokens(k.InputTokens), stats.FormatTokens(k.OutputTokens),
			stats.FormatTokens(k.CacheRead), stats.FormatTokens(k.CacheWrite), k.CacheHitRatio*100)
		fmt.Printf("Speed: weighted %s, p50 %s (%d timed) | Latency: p50 TTFT %s, p95 TTFT %s, p50 Dur %s\n",
			stats.FormatTPS(k.WeightedTPS), stats.FormatTPS(k.MedianTPS), k.SpeedSamples,
			stats.FormatMs(k.MedianTTFTMs), stats.FormatMs(k.P95TTFTMs), stats.FormatMs(k.MedianDurMs))
		fmt.Println("\nTop Models:")
		for i, r := range snap.Rankings {
			if i >= 12 {
				break
			}
			fmt.Printf("  %-34s %-9s -> %-24s %9s  %8s tok  %5d reqs\n",
				r.Key, r.MatchKind.String(), r.MatchedAs, stats.FormatUSD(r.CostUSD), stats.FormatTokens(r.Tokens), r.Requests)
		}
		if len(snap.Speeds) > 0 {
			fmt.Println("\nSpeed & Latency by Model:")
			for i, s := range snap.Speeds {
				if i >= 10 {
					break
				}
				fmt.Printf("  %-34s %10s (p50 %10s)  TTFT p50 %-8s p95 %-8s  n=%d\n",
					s.Model, stats.FormatTPS(s.WeightedTPS), stats.FormatTPS(s.MedianTPS),
					stats.FormatMs(s.MedianTTFTMs), stats.FormatMs(s.P95TTFTMs), s.Samples)
			}
		}
		if len(k.UnpricedModels) > 0 {
			fmt.Printf("\nUnpriced models (%d calls): %v\n", k.UnpricedCalls, k.UnpricedModels)
		}
		_ = model.AgentClaude
		return
	}

	if *refreshPrices {
		go func() {
			ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
			_ = cat.Refresh(ctx)
			cancel()
		}()
	}

	if err := gui.Run(sc, cat); err != nil {
		log.Fatal(err)
	}
}

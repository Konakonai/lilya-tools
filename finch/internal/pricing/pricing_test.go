package pricing

import (
	"math"
	"path/filepath"
	"testing"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

func TestCanonicalizeAndSuffixes(t *testing.T) {
	cases := []struct {
		raw   string
		canon string
	}{
		{"anthropic/claude-opus-5.5", "claude-opus-5-5"},
		{"claude-opus-5-5-20260801", "claude-opus-5-5"},
		{"claude-opus-5-5@20260801", "claude-opus-5-5"},
		{"antigravity/gemini-3.8-flash-n", "gemini-3-8-flash-n"},
		{"antigravity/gemini-3.8-flash-tiered", "gemini-3-8-flash-tiered"},
		{"codex/gpt-6.1-sol", "gpt-6-1-sol"},
		{"deepseek-v4p1-260910", "deepseek-v4-1"},
		{"claude-sonnet-5-5[1m]", "claude-sonnet-5-5"},
		{"google/gemini-3.8-flash:free", "gemini-3-8-flash"},
	}
	for _, tc := range cases {
		if got := Canonicalize(tc.raw); got != tc.canon {
			t.Errorf("Canonicalize(%q) = %q, want %q", tc.raw, got, tc.canon)
		}
	}
}

func TestCatalogResolveSuffixAndTiers(t *testing.T) {
	tmp := t.TempDir()
	c := NewCatalog(filepath.Join(tmp, "prices.json"), filepath.Join(tmp, "overrides.json"))
	c.replaceModels(map[string]ModelPrice{
		"gemini-3-8-flash": {
			ID:        "gemini-3.8-flash",
			Provider:  "google",
			Source:    "models.dev",
			Input:     0.5,
			Output:    3.0,
			CacheRead: 0.05,
		},
		"grok-4-7": {
			ID:        "grok-4.7",
			Provider:  "xai",
			Source:    "models.dev",
			Input:     2.0,
			Output:    6.0,
			CacheRead: 0.5,
			Tiers: []PriceTier{
				{ContextSize: 200_000, Input: 4.0, Output: 12.0, CacheRead: 1.0},
			},
		},
		"claude-opus-5-5": {
			ID:         "claude-opus-5-5",
			Provider:   "anthropic",
			Source:     "models.dev",
			Input:      5.0,
			Output:     25.0,
			CacheRead:  0.5,
			CacheWrite: 6.25,
		},
	}, time.Now(), "")

	// Exact match
	r := c.Resolve("claude-opus-5-5")
	if r.Kind != model.MatchExact || r.MatchedAs != "claude-opus-5-5" {
		t.Fatalf("exact match failed: %+v", r)
	}

	// Single-letter deployment suffix (-n)
	r = c.Resolve("antigravity/gemini-3.8-flash-n")
	if r.Kind != model.MatchSuffix || r.MatchedAs != "gemini-3.8-flash" {
		t.Fatalf("suffix -n failed: %+v", r)
	}

	// -tiered suffix
	r = c.Resolve("gemini-3.8-flash-tiered")
	if r.Kind != model.MatchSuffix || r.MatchedAs != "gemini-3.8-flash" {
		t.Fatalf("suffix -tiered failed: %+v", r)
	}

	// -build suffix + context tier > 200k
	r = c.Resolve("grok-4.7-build")
	if r.Kind != model.MatchSuffix || r.MatchedAs != "grok-4.7" {
		t.Fatalf("suffix -build failed: %+v", r)
	}
	costUnder := r.Price.ComputeUSD(model.Tokens{Input: 100_000, Output: 10_000})
	wantUnder := (100_000*2.0 + 10_000*6.0) / 1e6
	if math.Abs(costUnder-wantUnder) > 1e-9 {
		t.Fatalf("under-tier cost = %v, want %v", costUnder, wantUnder)
	}
	costOver := r.Price.ComputeUSD(model.Tokens{Input: 250_000, Output: 10_000})
	wantOver := (250_000*4.0 + 10_000*12.0) / 1e6
	if math.Abs(costOver-wantOver) > 1e-9 {
		t.Fatalf("over-tier cost = %v, want %v", costOver, wantOver)
	}

	// User override alias
	if err := c.SetOverride("gemini-4-argon-medium", OverrideEntry{Alias: "claude-opus-5-5"}); err != nil {
		t.Fatalf("SetOverride: %v", err)
	}
	r = c.Resolve("antigravity/gemini-4-argon-medium")
	if r.Kind != model.MatchOverride || r.MatchedAs != "claude-opus-5-5" {
		t.Fatalf("override alias failed: %+v", r)
	}
}

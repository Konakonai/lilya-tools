package pricing

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

const (
	ModelsDevURL  = "https://models.dev/api.json"
	OpenRouterURL = "https://openrouter.ai/api/v1/models"
	CacheTTL      = 24 * time.Hour
)

// PriceTier defines higher per-million pricing once a prompt exceeds ContextSize tokens.
type PriceTier struct {
	ContextSize int64   `json:"context_size"`
	Input       float64 `json:"input"`
	Output      float64 `json:"output"`
	CacheRead   float64 `json:"cache_read"`
	CacheWrite  float64 `json:"cache_write"`
}

// ModelPrice holds USD per 1,000,000 tokens.
type ModelPrice struct {
	ID         string      `json:"id"`
	Provider   string      `json:"provider"`
	Source     string      `json:"source"` // "models.dev", "openrouter", "override"
	Input      float64     `json:"input"`
	Output     float64     `json:"output"`
	CacheRead  float64     `json:"cache_read"`
	CacheWrite float64     `json:"cache_write"`
	Tiers      []PriceTier `json:"tiers,omitempty"`
}

// ComputeUSD calculates the call cost in USD, applying context-length tiers when present.
func (p ModelPrice) ComputeUSD(t model.Tokens) float64 {
	inRate, outRate, crRate, cwRate := p.Input, p.Output, p.CacheRead, p.CacheWrite
	if len(p.Tiers) > 0 {
		promptTotal := t.PromptTotal()
		var bestSize int64
		for _, tier := range p.Tiers {
			if tier.ContextSize > 0 && promptTotal > tier.ContextSize && tier.ContextSize >= bestSize {
				bestSize = tier.ContextSize
				if tier.Input > 0 {
					inRate = tier.Input
				}
				if tier.Output > 0 {
					outRate = tier.Output
				}
				if tier.CacheRead > 0 {
					crRate = tier.CacheRead
				}
				if tier.CacheWrite > 0 {
					cwRate = tier.CacheWrite
				}
			}
		}
	}
	const perM = 1_000_000.0
	return (float64(t.Input)*inRate +
		float64(t.Output)*outRate +
		float64(t.CacheRead)*crRate +
		float64(t.CacheWrite)*cwRate) / perM
}

// OverrideEntry lets users pin custom pricing or alias an internal codename to a known model.
type OverrideEntry struct {
	Alias      string  `json:"alias,omitempty"`
	Input      float64 `json:"input,omitempty"`
	Output     float64 `json:"output,omitempty"`
	CacheRead  float64 `json:"cache_read,omitempty"`
	CacheWrite float64 `json:"cache_write,omitempty"`
}

type cachedCatalog struct {
	UpdatedAt time.Time             `json:"updated_at"`
	Models    map[string]ModelPrice `json:"models"`
}

// Catalog resolves model names to USD prices with live fetching, disk caching,
// suffix stripping, prefix fallback, and user overrides.
type Catalog struct {
	mu           sync.RWMutex
	cachePath    string
	overridePath string
	updatedAt    time.Time
	models       map[string]ModelPrice // keyed by Canonicalize(id)
	overrides    map[string]OverrideEntry
	sortedKeys   []string // canonical keys sorted longest-first for prefix fallback
	lastErr      string
	httpClient   *http.Client
}

// Resolution describes how a raw model ID was matched.
type Resolution struct {
	Price     ModelPrice
	MatchedAs string
	Kind      model.MatchKind
}

// DefaultPaths returns standard XDG cache and config paths for finch pricing.
func DefaultPaths() (cachePath, overridePath string) {
	cacheDir, err := os.UserCacheDir()
	if err != nil || cacheDir == "" {
		home, _ := os.UserHomeDir()
		cacheDir = filepath.Join(home, ".cache")
	}
	cfgDir, err := os.UserConfigDir()
	if err != nil || cfgDir == "" {
		home, _ := os.UserHomeDir()
		cfgDir = filepath.Join(home, ".config")
	}
	return filepath.Join(cacheDir, "finch", "prices.json"),
		filepath.Join(cfgDir, "finch", "price-overrides.json")
}

// NewCatalog initializes a catalog from disk cache and user overrides.
func NewCatalog(cachePath, overridePath string) *Catalog {
	c := &Catalog{
		cachePath:    cachePath,
		overridePath: overridePath,
		models:       make(map[string]ModelPrice),
		overrides:    make(map[string]OverrideEntry),
		httpClient:   &http.Client{Timeout: 20 * time.Second},
	}
	c.loadDiskCache()
	c.LoadOverrides()
	return c
}

// Status returns cache metadata for display in the UI.
func (c *Catalog) Status() (count int, updatedAt time.Time, lastErr string) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return len(c.models), c.updatedAt, c.lastErr
}

// IsStale reports whether the disk cache is missing or older than CacheTTL.
func (c *Catalog) IsStale() bool {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return len(c.models) == 0 || time.Since(c.updatedAt) > CacheTTL
}

// LoadOverrides reads user-supplied price overrides from disk if present.
func (c *Catalog) LoadOverrides() {
	if c.overridePath == "" {
		return
	}
	b, err := os.ReadFile(c.overridePath)
	if err != nil {
		return
	}
	var raw map[string]OverrideEntry
	if json.Unmarshal(b, &raw) != nil {
		return
	}
	norm := make(map[string]OverrideEntry, len(raw))
	for k, v := range raw {
		ck := Canonicalize(k)
		if ck != "" {
			norm[ck] = v
		}
	}
	c.mu.Lock()
	c.overrides = norm
	c.mu.Unlock()
}

// SetOverride persists a single model alias or custom price to price-overrides.json.
func (c *Catalog) SetOverride(rawModel string, entry OverrideEntry) error {
	ck := Canonicalize(rawModel)
	if ck == "" {
		return errors.New("empty model name")
	}
	c.mu.Lock()
	c.overrides[ck] = entry
	snapshot := make(map[string]OverrideEntry, len(c.overrides))
	for k, v := range c.overrides {
		snapshot[k] = v
	}
	c.mu.Unlock()

	if c.overridePath == "" {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(c.overridePath), 0o755); err != nil {
		return err
	}
	b, err := json.MarshalIndent(snapshot, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(c.overridePath, b, 0o644)
}

func (c *Catalog) loadDiskCache() {
	if c.cachePath == "" {
		return
	}
	b, err := os.ReadFile(c.cachePath)
	if err != nil {
		return
	}
	var cc cachedCatalog
	if json.Unmarshal(b, &cc) != nil || len(cc.Models) == 0 {
		return
	}
	c.replaceModels(cc.Models, cc.UpdatedAt, "")
}

func (c *Catalog) saveDiskCache(models map[string]ModelPrice, updatedAt time.Time) {
	if c.cachePath == "" {
		return
	}
	if err := os.MkdirAll(filepath.Dir(c.cachePath), 0o755); err != nil {
		return
	}
	b, err := json.Marshal(cachedCatalog{UpdatedAt: updatedAt, Models: models})
	if err != nil {
		return
	}
	tmp := c.cachePath + ".tmp"
	if os.WriteFile(tmp, b, 0o644) == nil {
		_ = os.Rename(tmp, c.cachePath)
	}
}

func (c *Catalog) replaceModels(models map[string]ModelPrice, updatedAt time.Time, errMsg string) {
	keys := make([]string, 0, len(models))
	for k := range models {
		keys = append(keys, k)
	}
	// Longest keys first for longest-prefix fallback.
	sort.Slice(keys, func(i, j int) bool {
		if len(keys[i]) != len(keys[j]) {
			return len(keys[i]) > len(keys[j])
		}
		return keys[i] < keys[j]
	})
	c.mu.Lock()
	c.models = models
	c.sortedKeys = keys
	c.updatedAt = updatedAt
	c.lastErr = errMsg
	c.mu.Unlock()
}

// Refresh fetches fresh pricing from models.dev (primary) and OpenRouter (fallback),
// merges them with first-party priority, and updates the disk cache.
func (c *Catalog) Refresh(ctx context.Context) error {
	merged := make(map[string]ModelPrice, 1024)
	prio := make(map[string]int, 1024)

	var errs []string
	if md, err := c.fetchModelsDev(ctx); err != nil {
		errs = append(errs, "models.dev: "+err.Error())
	} else {
		for k, mp := range md {
			p := providerPriority(mp.Provider, k)
			if curP, ok := prio[k]; !ok || p > curP {
				merged[k] = mp
				prio[k] = p
			}
		}
	}

	if or, err := c.fetchOpenRouter(ctx); err != nil {
		errs = append(errs, "openrouter: "+err.Error())
	} else {
		for k, mp := range or {
			if _, exists := merged[k]; !exists {
				merged[k] = mp
				prio[k] = 1
			}
		}
	}

	if len(merged) == 0 {
		msg := strings.Join(errs, "; ")
		c.mu.Lock()
		c.lastErr = msg
		c.mu.Unlock()
		return errors.New(msg)
	}

	now := time.Now()
	c.replaceModels(merged, now, strings.Join(errs, "; "))
	c.saveDiskCache(merged, now)
	return nil
}

// Resolve finds the best matching ModelPrice for a raw model name.
func (c *Catalog) Resolve(raw string) Resolution {
	c.mu.RLock()
	defer c.mu.RUnlock()

	canon := Canonicalize(raw)
	if canon == "" {
		return Resolution{Kind: model.MatchNone}
	}

	// 1. Explicit user override (exact or after suffix stripping).
	if ov, ok := c.overrides[canon]; ok {
		return c.resolveOverrideLocked(canon, ov)
	}

	// 2. Exact match on canonical ID.
	if mp, ok := c.models[canon]; ok {
		return Resolution{Price: mp, MatchedAs: mp.ID, Kind: model.MatchExact}
	}

	// 3. Progressive suffix stripping.
	candidates := StripSuffixCandidates(raw)
	for i := 1; i < len(candidates); i++ {
		cand := candidates[i]
		if ov, ok := c.overrides[cand]; ok {
			return c.resolveOverrideLocked(cand, ov)
		}
		if mp, ok := c.models[cand]; ok {
			kind := model.MatchSuffix
			// If we had to chop tokens beyond known suffix peeling, mark as prefix fallback.
			if !strings.HasPrefix(canon, cand) {
				kind = model.MatchPrefix
			}
			return Resolution{Price: mp, MatchedAs: mp.ID, Kind: kind}
		}
	}

	// 4. Longest prefix match against known catalog keys (e.g. "grok-4-7-build-beta" -> "grok-4-7").
	for _, k := range c.sortedKeys {
		if len(k) < 5 {
			continue
		}
		if strings.HasPrefix(canon, k+"-") {
			mp := c.models[k]
			return Resolution{Price: mp, MatchedAs: mp.ID, Kind: model.MatchPrefix}
		}
	}

	return Resolution{Kind: model.MatchNone}
}

func (c *Catalog) resolveOverrideLocked(key string, ov OverrideEntry) Resolution {
	if ov.Alias != "" {
		target := Canonicalize(ov.Alias)
		if mp, ok := c.models[target]; ok {
			mp.Source = "override"
			return Resolution{Price: mp, MatchedAs: mp.ID, Kind: model.MatchOverride}
		}
	}
	return Resolution{
		Price: ModelPrice{
			ID:         key,
			Provider:   "override",
			Source:     "override",
			Input:      ov.Input,
			Output:     ov.Output,
			CacheRead:  ov.CacheRead,
			CacheWrite: ov.CacheWrite,
		},
		MatchedAs: key,
		Kind:      model.MatchOverride,
	}
}

// PriceCall computes and attaches cost metadata to a Call.
func (c *Catalog) PriceCall(call *model.Call) {
	res := c.Resolve(call.Model)
	if res.Kind != model.MatchNone {
		call.CostUSD = res.Price.ComputeUSD(call.Tokens)
		call.MatchedAs = res.MatchedAs
		call.MatchKind = res.Kind
		call.PriceSource = res.Price.Source
		return
	}
	if call.ReportedCost > 0 {
		call.CostUSD = call.ReportedCost
		call.MatchedAs = call.Model
		call.MatchKind = model.MatchReported
		call.PriceSource = "log"
		return
	}
	call.CostUSD = 0
	call.MatchedAs = ""
	call.MatchKind = model.MatchNone
	call.PriceSource = ""
}

// providerPriority prefers first-party vendors over third-party resellers/aggregators.
func providerPriority(provider, canonID string) int {
	p := strings.ToLower(provider)
	switch {
	case strings.HasPrefix(canonID, "claude-") && p == "anthropic":
		return 100
	case (strings.HasPrefix(canonID, "gpt-") || strings.HasPrefix(canonID, "o1") || strings.HasPrefix(canonID, "o3") || strings.HasPrefix(canonID, "o4") || strings.HasPrefix(canonID, "codex-")) && p == "openai":
		return 100
	case strings.HasPrefix(canonID, "gemini-") && p == "google":
		return 100
	case strings.HasPrefix(canonID, "grok-") && p == "xai":
		return 100
	case strings.HasPrefix(canonID, "deepseek-") && p == "deepseek":
		return 100
	case strings.HasPrefix(canonID, "kimi-") && (p == "moonshotai" || p == "moonshot"):
		return 100
	case strings.HasPrefix(canonID, "glm-") && (p == "zhipuai" || p == "zai"):
		return 100
	case strings.HasPrefix(canonID, "qwen") && (p == "alibaba" || p == "dashscope"):
		return 100
	case strings.HasPrefix(canonID, "minimax") && p == "minimax":
		return 100
	}
	switch p {
	case "anthropic", "openai", "google", "xai", "deepseek", "mistral", "cohere", "moonshotai", "zhipuai", "minimax":
		return 80
	case "google-vertex", "amazon-bedrock", "azure":
		return 50
	case "openrouter":
		return 30
	default:
		return 20
	}
}

func (c *Catalog) fetchModelsDev(ctx context.Context) (map[string]ModelPrice, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, ModelsDevURL, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", "finch/1.0")
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d", resp.StatusCode)
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, 32<<20))
	if err != nil {
		return nil, err
	}
	return parseModelsDevJSON(body)
}

type modelsDevProvider struct {
	Models map[string]modelsDevModel `json:"models"`
}

type modelsDevModel struct {
	ID   string         `json:"id"`
	Cost *modelsDevCost `json:"cost"`
}

type modelsDevCost struct {
	Input           float64         `json:"input"`
	Output          float64         `json:"output"`
	CacheRead       float64         `json:"cache_read"`
	CacheWrite      float64         `json:"cache_write"`
	ContextOver200k *modelsDevCost  `json:"context_over_200k"`
	Tiers           []modelsDevTier `json:"tiers"`
}

type modelsDevTier struct {
	Input      float64 `json:"input"`
	Output     float64 `json:"output"`
	CacheRead  float64 `json:"cache_read"`
	CacheWrite float64 `json:"cache_write"`
	Tier       struct {
		Type string `json:"type"`
		Size int64  `json:"size"`
	} `json:"tier"`
}

func parseModelsDevJSON(body []byte) (map[string]ModelPrice, error) {
	var raw map[string]modelsDevProvider
	if err := json.Unmarshal(body, &raw); err != nil {
		return nil, err
	}
	out := make(map[string]ModelPrice, 1024)
	prio := make(map[string]int, 1024)
	for provID, prov := range raw {
		for modelKey, m := range prov.Models {
			if m.Cost == nil {
				continue
			}
			if m.Cost.Input <= 0 && m.Cost.Output <= 0 {
				continue
			}
			id := m.ID
			if id == "" {
				id = modelKey
			}
			ck := Canonicalize(id)
			if ck == "" {
				continue
			}
			mp := ModelPrice{
				ID:         id,
				Provider:   provID,
				Source:     "models.dev",
				Input:      m.Cost.Input,
				Output:     m.Cost.Output,
				CacheRead:  m.Cost.CacheRead,
				CacheWrite: m.Cost.CacheWrite,
			}
			for _, t := range m.Cost.Tiers {
				if t.Tier.Type == "context" && t.Tier.Size > 0 {
					mp.Tiers = append(mp.Tiers, PriceTier{
						ContextSize: t.Tier.Size,
						Input:       t.Input,
						Output:      t.Output,
						CacheRead:   t.CacheRead,
						CacheWrite:  t.CacheWrite,
					})
				}
			}
			if len(mp.Tiers) == 0 && m.Cost.ContextOver200k != nil {
				ov := m.Cost.ContextOver200k
				mp.Tiers = append(mp.Tiers, PriceTier{
					ContextSize: 200_000,
					Input:       ov.Input,
					Output:      ov.Output,
					CacheRead:   ov.CacheRead,
					CacheWrite:  ov.CacheWrite,
				})
			}
			p := providerPriority(provID, ck)
			if curP, ok := prio[ck]; !ok || p > curP {
				out[ck] = mp
				prio[ck] = p
			}
		}
	}
	return out, nil
}

func (c *Catalog) fetchOpenRouter(ctx context.Context) (map[string]ModelPrice, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, OpenRouterURL, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", "finch/1.0")
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d", resp.StatusCode)
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, 16<<20))
	if err != nil {
		return nil, err
	}
	return parseOpenRouterJSON(body)
}

func parseOpenRouterJSON(body []byte) (map[string]ModelPrice, error) {
	var resp struct {
		Data []struct {
			ID      string            `json:"id"`
			Pricing map[string]string `json:"pricing"`
		} `json:"data"`
	}
	if err := json.Unmarshal(body, &resp); err != nil {
		return nil, err
	}
	out := make(map[string]ModelPrice, len(resp.Data))
	for _, m := range resp.Data {
		if m.ID == "" || len(m.Pricing) == 0 {
			continue
		}
		inPerTok, _ := strconv.ParseFloat(m.Pricing["prompt"], 64)
		outPerTok, _ := strconv.ParseFloat(m.Pricing["completion"], 64)
		crPerTok, _ := strconv.ParseFloat(m.Pricing["input_cache_read"], 64)
		cwPerTok, _ := strconv.ParseFloat(m.Pricing["input_cache_write"], 64)
		if inPerTok <= 0 && outPerTok <= 0 {
			continue
		}
		ck := Canonicalize(m.ID)
		if ck == "" {
			continue
		}
		if _, exists := out[ck]; exists {
			continue
		}
		prov := "openrouter"
		if p, _, ok := strings.Cut(m.ID, "/"); ok && p != "" {
			prov = p
		}
		out[ck] = ModelPrice{
			ID:         m.ID,
			Provider:   prov,
			Source:     "openrouter",
			Input:      inPerTok * 1_000_000,
			Output:     outPerTok * 1_000_000,
			CacheRead:  crPerTok * 1_000_000,
			CacheWrite: cwPerTok * 1_000_000,
		}
	}
	return out, nil
}

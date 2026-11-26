package model

import "time"

// Agent identifies which coding tool produced a usage event.
type Agent string

const (
	AgentClaude      Agent = "claude"
	AgentCodex       Agent = "codex"
	AgentAntigravity Agent = "antigravity"
	AgentPi          Agent = "pi"
	AgentGrok        Agent = "grok"
	AgentMagpie      Agent = "magpie"
)

// AllAgents defines display order for filters and legends.
var AllAgents = []Agent{
	AgentClaude,
	AgentCodex,
	AgentAntigravity,
	AgentPi,
	AgentGrok,
	AgentMagpie,
}

// AgentLabel returns a human-readable display name.
func AgentLabel(a Agent) string {
	switch a {
	case AgentClaude:
		return "Claude"
	case AgentCodex:
		return "Codex"
	case AgentAntigravity:
		return "Antigravity"
	case AgentPi:
		return "Pi"
	case AgentGrok:
		return "Grok"
	case AgentMagpie:
		return "Magpie"
	default:
		if a == "" {
			return "Other"
		}
		return string(a)
	}
}

// Tokens holds token counts for one model call.
// Input is ALWAYS non-cached prompt input tokens.
type Tokens struct {
	Input      int64 `json:"in"`
	Output     int64 `json:"out"`
	CacheRead  int64 `json:"cache_read"`
	CacheWrite int64 `json:"cache_write"`
	Reasoning  int64 `json:"reasoning,omitempty"`
}

// Total returns all billed tokens (input + output + cache read + cache write).
func (t Tokens) Total() int64 {
	return t.Input + t.Output + t.CacheRead + t.CacheWrite
}

// PromptTotal returns total prompt context tokens (input + cache read + cache write).
func (t Tokens) PromptTotal() int64 {
	return t.Input + t.CacheRead + t.CacheWrite
}

// Zero reports whether all token counts are zero.
func (t Tokens) Zero() bool {
	return t.Input <= 0 && t.Output <= 0 && t.CacheRead <= 0 && t.CacheWrite <= 0
}

// MatchKind describes how a model's price was resolved.
type MatchKind uint8

const (
	MatchNone     MatchKind = iota // Unpriced
	MatchExact                     // Exact normalized ID match
	MatchSuffix                    // Matched after stripping variant/date/deployment suffixes
	MatchPrefix                    // Fallback to longest matching base model prefix
	MatchOverride                  // Matched via user price-overrides.json
	MatchReported                  // Fell back to cost reported in the raw log (e.g., Grok ticks)
)

func (m MatchKind) String() string {
	switch m {
	case MatchExact:
		return "exact"
	case MatchSuffix:
		return "suffix"
	case MatchPrefix:
		return "prefix"
	case MatchOverride:
		return "override"
	case MatchReported:
		return "reported"
	default:
		return "unpriced"
	}
}

// Call represents a single deduplicated LLM inference event.
type Call struct {
	ID           string    `json:"id"`
	Timestamp    time.Time `json:"ts"`
	Agent        Agent     `json:"agent"`
	Source       string    `json:"source"` // parser origin: claude, codex, antigravity, agy-cli, pi, grok, magpie
	SessionID    string    `json:"session_id"`
	Project      string    `json:"project,omitempty"`
	Model        string    `json:"model"`
	ResponseID   string    `json:"response_id,omitempty"`
	Tokens       Tokens    `json:"tokens"`
	DurationMs   int64     `json:"duration_ms,omitempty"`
	TTFTMs       int64     `json:"ttft_ms,omitempty"`
	ReportedCost float64   `json:"reported_cost,omitempty"` // USD if log explicitly recorded cost

	// Computed at pricing/aggregation time (not stored in file cache).
	CostUSD     float64   `json:"-"`
	MatchedAs   string    `json:"-"`
	MatchKind   MatchKind `json:"-"`
	PriceSource string    `json:"-"`
}

// TPS returns output tokens per second for this call, or 0 when timing data
// is missing or too short to measure reliably.
//
// For gateway logs (source == "magpie"), TTFTMs is true HTTP first-byte latency
// so generation time excludes TTFT. For local agent session logs (Antigravity,
// Claude), TTFTMs marks first visible text after reasoning tokens have already
// been produced, so output tokens (which include reasoning) are divided by full
// DurationMs to avoid inflated spikes.
func (c Call) TPS() float64 {
	if c.Tokens.Output <= 0 || c.DurationMs < 200 {
		return 0
	}
	genMs := c.DurationMs
	if c.Source == "magpie" && c.TTFTMs > 0 && c.DurationMs > c.TTFTMs+100 {
		genMs = c.DurationMs - c.TTFTMs
	}
	if genMs < 200 {
		return 0
	}
	tps := float64(c.Tokens.Output) / (float64(genMs) / 1000.0)
	if tps > 1000 {
		return 0
	}
	return tps
}

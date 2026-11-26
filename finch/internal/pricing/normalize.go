package pricing

import (
	"regexp"
	"strings"
)

var (
	reSeparators  = regexp.MustCompile(`[\s_]+`)
	reMultiDash   = regexp.MustCompile(`-{2,}`)
	reBracketTag  = regexp.MustCompile(`\[[^\]]+\]$`)
	reVersionDot  = regexp.MustCompile(`(\d)[.p](\d)`)
	reDateDash8   = regexp.MustCompile(`(?:[-@])20\d{6}$`)
	reDateDash6   = regexp.MustCompile(`(?:[-@])(?:2[3-9]|3\d)(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])$`)
	reDateISO     = regexp.MustCompile(`-20\d{2}-\d{2}-\d{2}$`)
	reSingleAlpha = regexp.MustCompile(`-[a-z]$`)
)

// Known non-pricing variant suffixes stripped progressively when matching a
// model against live pricing catalogs. Order matters: longer compound suffixes
// come before shorter ones.
var variantSuffixes = []string{
	"-extended-thinking",
	"-thinking-high",
	"-thinking-medium",
	"-thinking-low",
	"-high-thinking",
	"-medium-thinking",
	"-low-thinking",
	"-reasoning-high",
	"-reasoning-medium",
	"-reasoning-low",
	"-experimental",
	"-non-thinking",
	"-nothinking",
	"-thinking",
	"-reasoning",
	"-computer-use",
	"-preview",
	"-latest",
	"-tiered",
	"-xhigh",
	"-medium",
	"-build",
	"-trial",
	"-alpha",
	"-beta",
	"-high",
	"-low",
	"-fast",
	"-chat",
	"-code",
	"-exp",
	"-dev",
	"-v1",
	"-v2",
	"-v3",
}

// Canonicalize produces a normalized base identifier while preserving meaningful
// variant tokens: lowercased, vendor prefix stripped, separators unified to '-',
// and snapshot dates removed.
func Canonicalize(raw string) string {
	s := strings.ToLower(strings.TrimSpace(raw))
	if s == "" {
		return ""
	}
	// Strip vendor/gateway prefixes such as "anthropic/", "antigravity/",
	// "accounts/fireworks/models/".
	if idx := strings.LastIndex(s, "/"); idx >= 0 {
		s = s[idx+1:]
	}
	// Strip trailing bracket context tags like "[1m]" or "[200k]".
	s = reBracketTag.ReplaceAllString(s, "")
	// Strip colon routing/tier tags like ":free", ":extended", ":thinking", ":beta".
	if idx := strings.IndexByte(s, ':'); idx > 0 {
		s = s[:idx]
	}
	s = strings.TrimSpace(s)
	s = reSeparators.ReplaceAllString(s, "-")
	s = reMultiDash.ReplaceAllString(s, "-")
	s = strings.Trim(s, "-")

	// Normalize version dots and Fireworks "p" notation (4.6 -> 4-6, v4p1 -> v4-1).
	for reVersionDot.MatchString(s) {
		s = reVersionDot.ReplaceAllString(s, "${1}-${2}")
	}

	// Strip trailing snapshot date stamps (-20260801, @20260801, -260910, -2025-04-14).
	s = stripDates(s)
	return s
}

func stripDates(s string) string {
	for {
		orig := s
		s = reDateISO.ReplaceAllString(s, "")
		s = reDateDash8.ReplaceAllString(s, "")
		s = reDateDash6.ReplaceAllString(s, "")
		s = strings.TrimRight(s, "-@")
		if s == orig {
			return s
		}
	}
}

// StripSuffixCandidates yields progressively simpler candidate model IDs by
// peeling known deployment/effort/channel suffixes and single-letter deployment
// tags (e.g. "gemini-3.8-flash-n" -> "gemini-3-8-flash", "grok-4.6-build" -> "grok-4-6",
// "gemini-4-argon-medium" -> "gemini-4-argon").
// The first element is always Canonicalize(raw).
func StripSuffixCandidates(raw string) []string {
	base := Canonicalize(raw)
	if base == "" {
		return nil
	}
	seen := map[string]bool{base: true}
	out := []string{base}
	add := func(v string) {
		v = strings.Trim(v, "-")
		if v != "" && !seen[v] {
			seen[v] = true
			out = append(out, v)
		}
	}

	// Also handle raw with preserved dots (some catalogs index "gpt-5.4" or "gemini-3.8-flash").
	dotted := normalizeKeepDots(raw)
	add(dotted)

	// Iteratively strip known variant suffixes, dates, and single-letter deployment tags.
	cur := base
	for range 5 {
		next := peelOnce(cur)
		if next == cur || next == "" {
			break
		}
		add(next)
		cur = next
	}

	// Also generate hyphen-trimmed ancestors down to at least 2 tokens
	// (e.g. "gemini-4-argon-trial" -> "gemini-4-argon" -> "gemini-4")
	// to support longest-prefix fallback.
	parts := strings.Split(cur, "-")
	for i := len(parts) - 1; i >= 2; i-- {
		add(strings.Join(parts[:i], "-"))
	}

	return out
}

func peelOnce(s string) string {
	// Single-letter deployment suffix like gemini-3-8-flash-n or gemini-4-argon-d.
	if reSingleAlpha.MatchString(s) {
		trimmed := s[:len(s)-2]
		if strings.Count(trimmed, "-") >= 1 {
			return stripDates(trimmed)
		}
	}
	for _, suf := range variantSuffixes {
		if strings.HasSuffix(s, suf) && len(s) > len(suf)+2 {
			return stripDates(strings.TrimSuffix(s, suf))
		}
	}
	// Try stripping dates again in case a suffix hid a date (e.g. model-20260501-preview).
	if d := stripDates(s); d != s {
		return d
	}
	return s
}

func normalizeKeepDots(raw string) string {
	s := strings.ToLower(strings.TrimSpace(raw))
	if idx := strings.LastIndex(s, "/"); idx >= 0 {
		s = s[idx+1:]
	}
	s = reBracketTag.ReplaceAllString(s, "")
	if idx := strings.IndexByte(s, ':'); idx > 0 {
		s = s[:idx]
	}
	s = reSeparators.ReplaceAllString(s, "-")
	s = reMultiDash.ReplaceAllString(s, "-")
	s = strings.Trim(s, "-")
	return stripDates(s)
}

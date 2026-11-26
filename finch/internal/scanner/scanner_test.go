package scanner

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

func TestClaudeParserDedupeAndDuration(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "sess1.jsonl")
	content := "" +
		`{"type":"api-request-shape","timestamp":"2026-11-25T04:18:57.000Z"}` + "\n" +
		`{"type":"assistant","timestamp":"2026-11-25T04:19:00.000Z","sessionId":"s1","cwd":"/home/lilya/proj","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":500,"cache_creation_input_tokens":200}}}` + "\n" +
		`{"type":"assistant","timestamp":"2026-11-25T04:19:05.000Z","sessionId":"s1","cwd":"/home/lilya/proj","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":100,"output_tokens":400,"cache_read_input_tokens":500,"cache_creation_input_tokens":200}}}` + "\n"
	if err := os.WriteFile(p, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	calls, err := parseClaudeFile(p)
	if err != nil {
		t.Fatal(err)
	}
	if len(calls) != 1 {
		t.Fatalf("expected 1 deduped call, got %d", len(calls))
	}
	c := calls[0]
	if c.Tokens.Output != 400 || c.Tokens.Input != 100 || c.Tokens.CacheRead != 500 {
		t.Fatalf("unexpected tokens: %+v", c.Tokens)
	}
	if c.DurationMs != 8000 || c.TTFTMs != 3000 {
		t.Fatalf("unexpected timing: dur=%d ttft=%d", c.DurationMs, c.TTFTMs)
	}
	// Non-gateway calls divide output by full duration (400 / 8s = 50 tok/s)
	// so reasoning tokens generated before first text don't inflate TPS.
	if c.TPS() != 50 {
		t.Fatalf("expected 50 tok/s, got %v", c.TPS())
	}
}

func TestCodexParserTokenUsageRecord(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "rollout.jsonl")
	content := "" +
		`{"timestamp":"2026-11-20T21:58:43.000Z","type":"session_meta","payload":{"session_id":"codex-s1","cwd":"/home/lilya/app"}}` + "\n" +
		`{"timestamp":"2026-11-20T21:58:44.000Z","type":"turn_context","payload":{"model":"gpt-6.2-astra"}}` + "\n" +
		`{"timestamp":"2026-11-20T21:58:49.000Z","type":"token_usage_record","payload":{"response_id":"resp_1","usage":{"input_tokens":10000,"cached_input_tokens":7000,"cache_write_input_tokens":0,"output_tokens":500}}}` + "\n" +
		`{"timestamp":"2026-11-20T21:58:49.100Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10000,"cached_input_tokens":7000,"output_tokens":500}}}}` + "\n"
	if err := os.WriteFile(p, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	calls, err := parseCodexFile(p)
	if err != nil {
		t.Fatal(err)
	}
	if len(calls) != 1 {
		t.Fatalf("expected 1 call, got %d", len(calls))
	}
	if calls[0].Tokens.Input != 3000 || calls[0].Tokens.CacheRead != 7000 || calls[0].ResponseID != "resp_1" {
		t.Fatalf("unexpected codex call: %+v", calls[0])
	}
	if calls[0].DurationMs != 5000 {
		t.Fatalf("expected 5000ms duration, got %d", calls[0].DurationMs)
	}
}

func TestMagpieDedupe(t *testing.T) {
	ts := time.Date(2026, 11, 20, 12, 0, 0, 0, time.UTC)
	direct := []model.Call{
		{ID: "codex:1", Timestamp: ts, Agent: model.AgentCodex, ResponseID: "resp_abc", Tokens: model.Tokens{Input: 100, Output: 50}},
		{ID: "pi:1", Timestamp: ts.Add(time.Minute), Agent: model.AgentPi, Tokens: model.Tokens{Input: 200, Output: 60, CacheRead: 1000}},
	}
	magpie := []model.Call{
		{ID: "magpie:1", Timestamp: ts, Agent: model.AgentCodex, Source: "magpie", ResponseID: "resp_abc", Tokens: model.Tokens{Input: 100, Output: 50}},
		{ID: "magpie:2", Timestamp: ts.Add(time.Minute + 3*time.Second), Agent: model.AgentPi, Source: "magpie", Tokens: model.Tokens{Input: 200, Output: 60, CacheRead: 1000}},
		{ID: "magpie:3", Timestamp: ts.Add(2 * time.Minute), Agent: model.AgentMagpie, Source: "magpie", Tokens: model.Tokens{Input: 500, Output: 80}},
	}
	merged, dedup := mergeAndDedupe(direct, magpie)
	if dedup != 2 || len(merged) != 3 {
		t.Fatalf("expected dedup=2 len=3, got dedup=%d len=%d", dedup, len(merged))
	}
}

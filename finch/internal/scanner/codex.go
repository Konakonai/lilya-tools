package scanner

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

type codexUsage struct {
	InputTokens     int64 `json:"input_tokens"`
	CachedInput     int64 `json:"cached_input_tokens"`
	CacheWriteInput int64 `json:"cache_write_input_tokens"`
	OutputTokens    int64 `json:"output_tokens"`
	ReasoningOutput int64 `json:"reasoning_output_tokens"`
	TotalTokens     int64 `json:"total_tokens"`
}

type codexLine struct {
	Timestamp string          `json:"timestamp"`
	Type      string          `json:"type"`
	Payload   json.RawMessage `json:"payload"`
}

func discoverCodexFiles(home string) []sourceFile {
	roots := []string{
		filepath.Join(home, ".codex", "sessions"),
		filepath.Join(home, ".codex", "archived_sessions"),
	}
	var files []sourceFile
	for _, root := range roots {
		_ = filepath.WalkDir(root, func(p string, d fs.DirEntry, err error) error {
			if err != nil || d.IsDir() {
				return nil
			}
			if !strings.HasSuffix(d.Name(), ".jsonl") {
				return nil
			}
			if fi, err := d.Info(); err == nil && fi.Size() > 0 {
				files = append(files, sourceFile{
					Path:    p,
					Source:  "codex",
					Size:    fi.Size(),
					MtimeNS: fi.ModTime().UnixNano(),
				})
			}
			return nil
		})
	}
	return files
}

func parseCodexFile(path string) ([]model.Call, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	sessionID := strings.TrimSuffix(filepath.Base(path), ".jsonl")
	project := ""
	currentModel := ""

	var prevTotal codexUsage
	hasPrevTotal := false
	var lastTurnStart time.Time
	var recordCalls []model.Call
	var eventCalls []model.Call

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 16*1024*1024)
	seqRec := 0
	seqEvt := 0
	for sc.Scan() {
		b := sc.Bytes()
		if len(b) == 0 {
			continue
		}
		var ln codexLine
		if json.Unmarshal(b, &ln) != nil {
			continue
		}
		ts, tsErr := time.Parse(time.RFC3339Nano, ln.Timestamp)

		switch ln.Type {
		case "session_meta":
			var meta struct {
				ID        string `json:"id"`
				SessionID string `json:"session_id"`
				Cwd       string `json:"cwd"`
				Model     string `json:"model"`
			}
			if json.Unmarshal(ln.Payload, &meta) == nil {
				if meta.SessionID != "" {
					sessionID = meta.SessionID
				} else if meta.ID != "" {
					sessionID = meta.ID
				}
				if meta.Cwd != "" {
					project = filepath.Base(meta.Cwd)
				}
				if meta.Model != "" {
					currentModel = meta.Model
				}
			}
		case "turn_context":
			if tsErr == nil {
				lastTurnStart = ts
			}
			var tc struct {
				Cwd               string `json:"cwd"`
				Model             string `json:"model"`
				CollaborationMode *struct {
					Settings *struct {
						Model string `json:"model"`
					} `json:"settings"`
				} `json:"collaboration_mode"`
			}
			if json.Unmarshal(ln.Payload, &tc) == nil {
				if tc.Cwd != "" {
					project = filepath.Base(tc.Cwd)
				}
				if tc.Model != "" {
					currentModel = tc.Model
				} else if tc.CollaborationMode != nil && tc.CollaborationMode.Settings != nil && tc.CollaborationMode.Settings.Model != "" {
					currentModel = tc.CollaborationMode.Settings.Model
				}
			}
		case "response_item":
			if tsErr == nil && strings.Contains(string(ln.Payload), `"custom_tool_call_output"`) {
				lastTurnStart = ts
			}
		case "token_usage_record":
			if tsErr != nil {
				continue
			}
			var rec struct {
				ResponseID string      `json:"response_id"`
				Model      string      `json:"model"`
				Usage      *codexUsage `json:"usage"`
			}
			if json.Unmarshal(ln.Payload, &rec) != nil || rec.Usage == nil {
				continue
			}
			u := *rec.Usage
			toks := model.Tokens{
				Input:      max(0, u.InputTokens-u.CachedInput-u.CacheWriteInput),
				Output:     u.OutputTokens,
				CacheRead:  u.CachedInput,
				CacheWrite: u.CacheWriteInput,
				Reasoning:  u.ReasoningOutput,
			}
			if toks.Zero() {
				continue
			}
			mName := rec.Model
			if mName == "" {
				mName = currentModel
			}
			if mName == "" {
				mName = "codex"
			}
			var durMs int64
			if !lastTurnStart.IsZero() && ts.After(lastTurnStart) {
				d := ts.Sub(lastTurnStart).Milliseconds()
				if d >= 100 && d <= 10*60*1000 {
					durMs = d
				}
			}
			lastTurnStart = ts
			seqRec++
			id := fmt.Sprintf("codex:%s:r%d", sessionID, seqRec)
			if rec.ResponseID != "" {
				id = "codex:" + rec.ResponseID
			}
			recordCalls = append(recordCalls, model.Call{
				ID:         id,
				Timestamp:  ts,
				Agent:      model.AgentCodex,
				Source:     "codex",
				SessionID:  sessionID,
				Project:    project,
				Model:      mName,
				ResponseID: rec.ResponseID,
				Tokens:     toks,
				DurationMs: durMs,
			})
		case "event_msg":
			if tsErr != nil {
				continue
			}
			var ev struct {
				Type string `json:"type"`
				Info *struct {
					Model           string      `json:"model"`
					TotalTokenUsage *codexUsage `json:"total_token_usage"`
					LastTokenUsage  *codexUsage `json:"last_token_usage"`
				} `json:"info"`
			}
			if json.Unmarshal(ln.Payload, &ev) != nil || ev.Type != "token_count" || ev.Info == nil {
				continue
			}
			if ev.Info.Model != "" {
				currentModel = ev.Info.Model
			}
			var delta codexUsage
			if ev.Info.TotalTokenUsage != nil {
				cur := *ev.Info.TotalTokenUsage
				if hasPrevTotal && cur == prevTotal {
					continue
				}
				if hasPrevTotal && cur.InputTokens >= prevTotal.InputTokens && cur.OutputTokens >= prevTotal.OutputTokens {
					delta = codexUsage{
						InputTokens:     cur.InputTokens - prevTotal.InputTokens,
						CachedInput:     max(0, cur.CachedInput-prevTotal.CachedInput),
						CacheWriteInput: max(0, cur.CacheWriteInput-prevTotal.CacheWriteInput),
						OutputTokens:    cur.OutputTokens - prevTotal.OutputTokens,
						ReasoningOutput: max(0, cur.ReasoningOutput-prevTotal.ReasoningOutput),
					}
				} else if ev.Info.LastTokenUsage != nil {
					delta = *ev.Info.LastTokenUsage
				} else {
					delta = cur
				}
				prevTotal = cur
				hasPrevTotal = true
			} else if ev.Info.LastTokenUsage != nil {
				delta = *ev.Info.LastTokenUsage
			} else {
				continue
			}

			toks := model.Tokens{
				Input:      max(0, delta.InputTokens-delta.CachedInput-delta.CacheWriteInput),
				Output:     delta.OutputTokens,
				CacheRead:  delta.CachedInput,
				CacheWrite: delta.CacheWriteInput,
				Reasoning:  delta.ReasoningOutput,
			}
			if toks.Zero() {
				continue
			}
			mName := currentModel
			if mName == "" {
				mName = "codex"
			}
			var durMs int64
			if !lastTurnStart.IsZero() && ts.After(lastTurnStart) {
				d := ts.Sub(lastTurnStart).Milliseconds()
				if d >= 100 && d <= 10*60*1000 {
					durMs = d
				}
			}
			seqEvt++
			eventCalls = append(eventCalls, model.Call{
				ID:         fmt.Sprintf("codex:%s:%d", sessionID, seqEvt),
				Timestamp:  ts,
				Agent:      model.AgentCodex,
				Source:     "codex",
				SessionID:  sessionID,
				Project:    project,
				Model:      mName,
				Tokens:     toks,
				DurationMs: durMs,
			})
		}
	}

	// Newer Codex logs emit both token_usage_record (with exact response_id) and
	// event_msg:token_count; prefer token_usage_record when present to avoid double counting.
	if len(recordCalls) > 0 {
		return recordCalls, nil
	}
	return eventCalls, nil
}

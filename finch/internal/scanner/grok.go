package scanner

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io/fs"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

type grokSummary struct {
	Info struct {
		ID              string `json:"id"`
		CurrentModelID  string `json:"current_model_id"`
		ParentSessionID string `json:"parent_session_id"`
		CreatedAt       string `json:"created_at"`
	} `json:"info"`
}

type grokModelUsage struct {
	InputTokens         int64 `json:"inputTokens"`
	OutputTokens        int64 `json:"outputTokens"`
	CachedReadTokens    int64 `json:"cachedReadTokens"`
	CacheCreationTokens int64 `json:"cacheCreationTokens"`
	ReasoningTokens     int64 `json:"reasoningTokens"`
	CostUsdTicks        int64 `json:"costUsdTicks"`
}

type grokUpdateLine struct {
	Timestamp float64 `json:"timestamp"`
	Params    struct {
		SessionID string `json:"sessionId"`
		Update    struct {
			SessionUpdate string `json:"sessionUpdate"`
			PromptID      string `json:"prompt_id"`
			Usage         *struct {
				InputTokens         int64                     `json:"inputTokens"`
				OutputTokens        int64                     `json:"outputTokens"`
				CachedReadTokens    int64                     `json:"cachedReadTokens"`
				CacheCreationTokens int64                     `json:"cacheCreationTokens"`
				ReasoningTokens     int64                     `json:"reasoningTokens"`
				APIDurationMs       int64                     `json:"apiDurationMs"`
				CostUsdTicks        int64                     `json:"costUsdTicks"`
				ModelUsage          map[string]grokModelUsage `json:"modelUsage"`
			} `json:"usage"`
		} `json:"update"`
	} `json:"params"`
}

func discoverGrokFiles(home string) []sourceFile {
	root := filepath.Join(home, ".grok", "sessions")
	var files []sourceFile
	_ = filepath.WalkDir(root, func(p string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return nil
		}
		if d.Name() != "updates.jsonl" {
			return nil
		}
		if fi, err := d.Info(); err == nil && fi.Size() > 0 {
			files = append(files, sourceFile{
				Path:    p,
				Source:  "grok",
				Size:    fi.Size(),
				MtimeNS: fi.ModTime().UnixNano(),
			})
		}
		return nil
	})
	return files
}

func parseGrokFile(path string) ([]model.Call, error) {
	dir := filepath.Dir(path)
	sessionID := filepath.Base(dir)
	project := ""
	if rawProj := filepath.Base(filepath.Dir(dir)); rawProj != "" {
		if dec, err := url.PathUnescape(rawProj); err == nil {
			project = filepath.Base(dec)
		} else {
			project = rawProj
		}
	}

	var fallbackModel string
	var since time.Time
	if sb, err := os.ReadFile(filepath.Join(dir, "summary.json")); err == nil {
		var s grokSummary
		if json.Unmarshal(sb, &s) == nil {
			fallbackModel = s.Info.CurrentModelID
			if s.Info.ParentSessionID != "" && s.Info.CreatedAt != "" {
				if t, err := time.Parse(time.RFC3339Nano, s.Info.CreatedAt); err == nil {
					since = t.Truncate(time.Second)
				}
			}
		}
	}

	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	var out []model.Call
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 32*1024*1024)
	seq := 0
	for sc.Scan() {
		b := sc.Bytes()
		if len(b) == 0 || !strings.Contains(string(b), `"turn_completed"`) {
			continue
		}
		var ln grokUpdateLine
		if json.Unmarshal(b, &ln) != nil {
			continue
		}
		u := ln.Params.Update
		if u.SessionUpdate != "turn_completed" || u.Usage == nil || ln.Timestamp <= 0 {
			continue
		}
		ts := time.Unix(int64(ln.Timestamp), 0)
		if !since.IsZero() && ts.Before(since) {
			continue
		}
		sess := ln.Params.SessionID
		if sess == "" {
			sess = sessionID
		}
		usg := u.Usage
		if len(usg.ModelUsage) > 0 {
			keys := make([]string, 0, len(usg.ModelUsage))
			for k := range usg.ModelUsage {
				keys = append(keys, k)
			}
			sort.Strings(keys)
			for _, mName := range keys {
				mu := usg.ModelUsage[mName]
				toks := model.Tokens{
					Input:      max(0, mu.InputTokens-mu.CachedReadTokens-mu.CacheCreationTokens),
					Output:     mu.OutputTokens,
					CacheRead:  mu.CachedReadTokens,
					CacheWrite: mu.CacheCreationTokens,
					Reasoning:  mu.ReasoningTokens,
				}
				if toks.Zero() {
					continue
				}
				ticks := mu.CostUsdTicks
				if ticks == 0 && len(keys) == 1 {
					ticks = usg.CostUsdTicks
				}
				seq++
				out = append(out, model.Call{
					ID:           fmt.Sprintf("grok:%s:%d:%s", sess, seq, mName),
					Timestamp:    ts,
					Agent:        model.AgentGrok,
					Source:       "grok",
					SessionID:    sess,
					Project:      project,
					Model:        mName,
					ResponseID:   u.PromptID,
					Tokens:       toks,
					DurationMs:   usg.APIDurationMs,
					ReportedCost: float64(ticks) / 1e10,
				})
			}
		} else {
			toks := model.Tokens{
				Input:      max(0, usg.InputTokens-usg.CachedReadTokens-usg.CacheCreationTokens),
				Output:     usg.OutputTokens,
				CacheRead:  usg.CachedReadTokens,
				CacheWrite: usg.CacheCreationTokens,
				Reasoning:  usg.ReasoningTokens,
			}
			if toks.Zero() {
				continue
			}
			mName := fallbackModel
			if mName == "" {
				mName = "grok"
			}
			seq++
			out = append(out, model.Call{
				ID:           fmt.Sprintf("grok:%s:%d", sess, seq),
				Timestamp:    ts,
				Agent:        model.AgentGrok,
				Source:       "grok",
				SessionID:    sess,
				Project:      project,
				Model:        mName,
				ResponseID:   u.PromptID,
				Tokens:       toks,
				DurationMs:   usg.APIDurationMs,
				ReportedCost: float64(usg.CostUsdTicks) / 1e10,
			})
		}
	}
	return out, nil
}

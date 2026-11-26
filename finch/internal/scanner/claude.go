package scanner

import (
	"bufio"
	"encoding/json"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

type claudeLine struct {
	Type               string `json:"type"`
	Timestamp          string `json:"timestamp"`
	SessionID          string `json:"sessionId"`
	RequestID          string `json:"requestId"`
	Cwd                string `json:"cwd"`
	ThinkingDurationMs int64  `json:"thinkingDurationMs"`
	Message            *struct {
		ID    string `json:"id"`
		Role  string `json:"role"`
		Model string `json:"model"`
		Usage *struct {
			InputTokens         int64 `json:"input_tokens"`
			OutputTokens        int64 `json:"output_tokens"`
			CacheReadTokens     int64 `json:"cache_read_input_tokens"`
			CacheCreationTokens int64 `json:"cache_creation_input_tokens"`
			OutputDetails       *struct {
				ThinkingTokens int64 `json:"thinking_tokens"`
			} `json:"output_tokens_details"`
		} `json:"usage"`
	} `json:"message"`
}

func discoverClaudeFiles(home string) []sourceFile {
	root := filepath.Join(home, ".claude", "projects")
	var files []sourceFile
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
				Source:  "claude",
				Size:    fi.Size(),
				MtimeNS: fi.ModTime().UnixNano(),
			})
		}
		return nil
	})
	return files
}

func parseClaudeFile(path string) ([]model.Call, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	fallbackSession := strings.TrimSuffix(filepath.Base(path), ".jsonl")
	fallbackProject := cleanClaudeProjectDir(filepath.Base(filepath.Dir(path)))

	type tracked struct {
		call     model.Call
		reqStart time.Time
		firstAt  time.Time
		lastAt   time.Time
	}

	byKey := make(map[string]*tracked)
	order := make([]string, 0, 64)
	var lastPromptAt time.Time

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 16*1024*1024)
	for sc.Scan() {
		b := sc.Bytes()
		if len(b) == 0 {
			continue
		}
		s := string(b)
		isPromptMarker := strings.Contains(s, `"api-request-shape"`) || strings.Contains(s, `"type":"user"`) || strings.Contains(s, `"type": "user"`)
		hasUsage := strings.Contains(s, `"usage"`)
		if !isPromptMarker && !hasUsage {
			continue
		}

		var ln claudeLine
		if json.Unmarshal(b, &ln) != nil {
			continue
		}
		ts, tsErr := time.Parse(time.RFC3339Nano, ln.Timestamp)
		if ln.Type == "api-request-shape" || ln.Type == "user" {
			if tsErr == nil {
				lastPromptAt = ts
			}
			continue
		}
		if !hasUsage || tsErr != nil || ln.Message == nil || ln.Message.Usage == nil {
			continue
		}

		u := ln.Message.Usage
		toks := model.Tokens{
			Input:      u.InputTokens,
			Output:     u.OutputTokens,
			CacheRead:  u.CacheReadTokens,
			CacheWrite: u.CacheCreationTokens,
		}
		if u.OutputDetails != nil {
			toks.Reasoning = u.OutputDetails.ThinkingTokens
		}
		if toks.Zero() {
			continue
		}
		sess := ln.SessionID
		if sess == "" {
			sess = fallbackSession
		}
		proj := fallbackProject
		if ln.Cwd != "" {
			proj = filepath.Base(ln.Cwd)
		}
		key := ln.Message.ID
		if key == "" {
			key = ln.RequestID
		}
		if key == "" {
			key = sess + ":" + ln.Timestamp
		}

		call := model.Call{
			ID:         "claude:" + key,
			Timestamp:  ts,
			Agent:      model.AgentClaude,
			Source:     "claude",
			SessionID:  sess,
			Project:    proj,
			Model:      ln.Message.Model,
			ResponseID: ln.Message.ID,
			Tokens:     toks,
		}
		if prev, exists := byKey[key]; exists {
			if ts.After(prev.lastAt) {
				prev.lastAt = ts
			}
			if call.Tokens.Output >= prev.call.Tokens.Output && call.Tokens.Total() >= prev.call.Tokens.Total() {
				prev.call = call
				prev.call.Timestamp = prev.firstAt
			}
		} else {
			tr := &tracked{
				call:     call,
				reqStart: lastPromptAt,
				firstAt:  ts,
				lastAt:   ts,
			}
			byKey[key] = tr
			order = append(order, key)
		}
	}

	out := make([]model.Call, 0, len(order))
	for _, k := range order {
		tr := byKey[k]
		if !tr.reqStart.IsZero() && tr.lastAt.After(tr.reqStart) {
			dur := tr.lastAt.Sub(tr.reqStart).Milliseconds()
			if dur >= 100 && dur <= 10*60*1000 {
				tr.call.DurationMs = dur
				// First assistant line arrives after TTFT + first block, so only
				// estimate TTFT when a subsequent streaming block followed.
				if tr.lastAt.After(tr.firstAt) && tr.firstAt.After(tr.reqStart) {
					ttft := tr.firstAt.Sub(tr.reqStart).Milliseconds()
					if ttft > 0 && ttft < dur {
						tr.call.TTFTMs = ttft
					}
				}
			}
		} else if tr.lastAt.After(tr.firstAt) {
			dur := tr.lastAt.Sub(tr.firstAt).Milliseconds()
			if dur >= 100 && dur <= 10*60*1000 {
				tr.call.DurationMs = dur
			}
		}
		out = append(out, tr.call)
	}
	return out, nil
}

func cleanClaudeProjectDir(dir string) string {
	if dir == "" || dir == "subagents" {
		return ""
	}
	parts := strings.Split(strings.TrimPrefix(dir, "-"), "-")
	if len(parts) > 0 {
		return parts[len(parts)-1]
	}
	return dir
}

func bytesContainsUsage(b []byte) bool {
	return strings.Contains(string(b), `"usage"`)
}

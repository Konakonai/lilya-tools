package scanner

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

type magpieUsageRow struct {
	RouteID    int64  `json:"route_id"`
	Time       string `json:"t"`
	Agent      string `json:"agent"`
	Provider   string `json:"provider"`
	Model      string `json:"model"`
	Served     string `json:"served"`
	In         int64  `json:"in"`
	Out        int64  `json:"out"`
	CacheRead  int64  `json:"cache_read"`
	CacheWrite int64  `json:"cache_write"`
	Ms         int64  `json:"ms"`
	TTFTMs     int64  `json:"ttft_ms"`
	Status     int    `json:"status"`
	ResponseID string `json:"response_id"`
}

func discoverMagpieFiles(home string) []sourceFile {
	candidates := []string{
		filepath.Join(home, ".config", "magpie", "usage.jsonl"),
		filepath.Join(home, ".local", "share", "magpie", "usage.jsonl"),
	}
	var files []sourceFile
	for _, p := range candidates {
		if fi, err := os.Stat(p); err == nil && !fi.IsDir() && fi.Size() > 0 {
			files = append(files, sourceFile{
				Path:    p,
				Source:  "magpie",
				Size:    fi.Size(),
				MtimeNS: fi.ModTime().UnixNano(),
			})
		}
	}
	return files
}

func parseMagpieFile(path string) ([]model.Call, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	var out []model.Call
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 8*1024*1024)
	seq := 0
	for sc.Scan() {
		b := sc.Bytes()
		if len(b) == 0 {
			continue
		}
		var r magpieUsageRow
		if json.Unmarshal(b, &r) != nil {
			continue
		}
		if r.Status >= 400 {
			continue
		}
		toks := model.Tokens{
			Input:      r.In,
			Output:     r.Out,
			CacheRead:  r.CacheRead,
			CacheWrite: r.CacheWrite,
		}
		if toks.Zero() {
			continue
		}
		mName := strings.TrimSpace(r.Served)
		if mName == "" {
			mName = strings.TrimSpace(r.Model)
		}
		if mName == "" {
			continue
		}
		ts, err := time.Parse(time.RFC3339Nano, r.Time)
		if err != nil {
			continue
		}
		seq++
		id := fmt.Sprintf("magpie:%d:%d", r.RouteID, seq)
		out = append(out, model.Call{
			ID:         id,
			Timestamp:  ts,
			Agent:      mapMagpieAgent(r.Agent),
			Source:     "magpie",
			SessionID:  "magpie:" + r.Agent,
			Project:    r.Provider,
			Model:      mName,
			ResponseID: r.ResponseID,
			Tokens:     toks,
			DurationMs: r.Ms,
			TTFTMs:     r.TTFTMs,
		})
	}
	return out, nil
}

func mapMagpieAgent(a string) model.Agent {
	switch strings.ToLower(strings.TrimSpace(a)) {
	case "claude", "claude-code", "claude-desktop":
		return model.AgentClaude
	case "codex", "codex-desktop":
		return model.AgentCodex
	case "antigravity", "agy":
		return model.AgentAntigravity
	case "pi":
		return model.AgentPi
	case "grok", "grok-build":
		return model.AgentGrok
	default:
		return model.AgentMagpie
	}
}

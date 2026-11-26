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

type piLine struct {
	Type      string `json:"type"`
	ID        string `json:"id"`
	Timestamp string `json:"timestamp"`
	Message   *struct {
		Role     string `json:"role"`
		Provider string `json:"provider"`
		Model    string `json:"model"`
		Usage    *struct {
			Input      int64 `json:"input"`
			Output     int64 `json:"output"`
			CacheRead  int64 `json:"cacheRead"`
			CacheWrite int64 `json:"cacheWrite"`
			Reasoning  int64 `json:"reasoning"`
			Cost       *struct {
				Total float64 `json:"total"`
			} `json:"cost"`
		} `json:"usage"`
	} `json:"message"`
}

func discoverPiFiles(home string) []sourceFile {
	root := filepath.Join(home, ".pi", "agent", "sessions")
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
				Source:  "pi",
				Size:    fi.Size(),
				MtimeNS: fi.ModTime().UnixNano(),
			})
		}
		return nil
	})
	return files
}

func parsePiFile(path string) ([]model.Call, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	sessionID := strings.TrimSuffix(filepath.Base(path), ".jsonl")
	project := cleanPiProjectDir(filepath.Base(filepath.Dir(path)))

	var out []model.Call
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 16*1024*1024)
	seq := 0
	for sc.Scan() {
		b := sc.Bytes()
		if len(b) == 0 || !bytesContainsUsage(b) {
			continue
		}
		var ln piLine
		if json.Unmarshal(b, &ln) != nil || ln.Message == nil || ln.Message.Usage == nil {
			continue
		}
		u := ln.Message.Usage
		toks := model.Tokens{
			Input:      u.Input,
			Output:     u.Output,
			CacheRead:  u.CacheRead,
			CacheWrite: u.CacheWrite,
			Reasoning:  u.Reasoning,
		}
		if toks.Zero() {
			continue
		}
		ts, err := time.Parse(time.RFC3339Nano, ln.Timestamp)
		if err != nil {
			continue
		}
		var reported float64
		if u.Cost != nil && u.Cost.Total > 0 {
			reported = u.Cost.Total
		}
		seq++
		id := ln.ID
		if id == "" {
			id = fmt.Sprintf("%s:%d", sessionID, seq)
		}
		out = append(out, model.Call{
			ID:           "pi:" + sessionID + ":" + id,
			Timestamp:    ts,
			Agent:        model.AgentPi,
			Source:       "pi",
			SessionID:    sessionID,
			Project:      project,
			Model:        ln.Message.Model,
			Tokens:       toks,
			ReportedCost: reported,
		})
	}
	return out, nil
}

func cleanPiProjectDir(dir string) string {
	dir = strings.Trim(dir, "-")
	if dir == "" {
		return ""
	}
	parts := strings.Split(dir, "-")
	return parts[len(parts)-1]
}

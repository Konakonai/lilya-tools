package scanner

import (
	"database/sql"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

var errPB = errors.New("malformed protobuf")

func discoverAntigravityFiles(home string) []sourceFile {
	dirs := []struct {
		dir    string
		source string
	}{
		{filepath.Join(home, ".gemini", "antigravity", "conversations"), "antigravity"},
		{filepath.Join(home, ".gemini", "antigravity-insiders", "conversations"), "antigravity"},
		{filepath.Join(home, ".gemini", "antigravity-cli", "conversations"), "agy-cli"},
	}
	var files []sourceFile
	for _, d := range dirs {
		entries, err := os.ReadDir(d.dir)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if e.IsDir() || !strings.HasSuffix(e.Name(), ".db") {
				continue
			}
			p := filepath.Join(d.dir, e.Name())
			fi, err := os.Stat(p)
			if err != nil || fi.Size() == 0 {
				continue
			}
			size := fi.Size()
			mtime := fi.ModTime().UnixNano()
			// Include WAL file in fingerprint so active conversations refresh cleanly.
			if wfi, err := os.Stat(p + "-wal"); err == nil {
				size += wfi.Size()
				if wt := wfi.ModTime().UnixNano(); wt > mtime {
					mtime = wt
				}
			}
			files = append(files, sourceFile{
				Path:    p,
				Source:  d.source,
				Size:    size,
				MtimeNS: mtime,
			})
		}
	}
	return files
}

func parseAntigravityFile(path, source string) ([]model.Call, error) {
	dsn := fmt.Sprintf("file:%s?mode=ro&_busy_timeout=1500", path)
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, err
	}
	defer db.Close()

	models := make(map[int64]string)
	if rows, err := db.Query("SELECT idx, data FROM gen_metadata WHERE data IS NOT NULL"); err == nil {
		for rows.Next() {
			var idx int64
			var b []byte
			if rows.Scan(&idx, &b) == nil {
				if m := agParseModel(b); m != "" {
					models[idx] = m
				}
			}
		}
		rows.Close()
	}

	rows, err := db.Query("SELECT idx, metadata FROM steps WHERE metadata IS NOT NULL ORDER BY idx")
	if err != nil {
		return nil, nil
	}
	defer rows.Close()

	sessionID := strings.TrimSuffix(filepath.Base(path), ".db")
	var out []model.Call
	byResp := make(map[string]int)

	for rows.Next() {
		var idx int64
		var b []byte
		if rows.Scan(&idx, &b) != nil {
			continue
		}
		call, genIdx, ok := agParseStep(b)
		if !ok {
			continue
		}
		mName := models[genIdx]
		if mName == "" {
			mName = "antigravity"
		}
		call.ID = fmt.Sprintf("%s:%s:%d", source, sessionID, idx)
		call.Agent = model.AgentAntigravity
		call.Source = source
		call.SessionID = sessionID
		call.Model = mName

		if call.ResponseID != "" {
			if pos, seen := byResp[call.ResponseID]; seen {
				// Keep earliest timestamp/TTFT while updating to the latest token totals.
				if !out[pos].Timestamp.IsZero() && out[pos].Timestamp.Before(call.Timestamp) {
					call.Timestamp = out[pos].Timestamp
				}
				if out[pos].TTFTMs > 0 && call.TTFTMs == 0 {
					call.TTFTMs = out[pos].TTFTMs
				}
				out[pos] = call
				continue
			}
			byResp[call.ResponseID] = len(out)
		}
		out = append(out, call)
	}
	return out, nil
}

// agParseStep decodes exa.cortex_pb.CortexStepMetadata protobuf fields:
//
//	1:  created_at (google.protobuf.Timestamp)
//	6:  viewable_at
//	7:  finished_generating_at
//	9:  usage (2=input non-cached, 3=output, 4=cache_write, 5=cache_read, 11=response_id)
//	20: location -> 3=gen_metadata idx
func agParseStep(b []byte) (call model.Call, genIdx int64, ok bool) {
	var usage []byte
	var began, first, ended time.Time
	err := pbWalk(b, func(num int, v uint64, d []byte) {
		switch num {
		case 1:
			if d != nil {
				began = pbTime(d)
			}
		case 6:
			if d != nil {
				first = pbTime(d)
			}
		case 7:
			if d != nil {
				ended = pbTime(d)
			}
		case 9:
			usage = d
		case 20:
			if d != nil {
				_ = pbWalk(d, func(n int, v uint64, d []byte) {
					if n == 3 && d == nil {
						genIdx = int64(v)
					}
				})
			}
		}
	})
	if err != nil || usage == nil || began.IsZero() {
		return call, 0, false
	}

	var toks model.Tokens
	var respID string
	err = pbWalk(usage, func(num int, v uint64, d []byte) {
		if d == nil {
			switch num {
			case 2:
				toks.Input = int64(v)
			case 3:
				toks.Output = int64(v)
			case 4:
				toks.CacheWrite = int64(v)
			case 5:
				toks.CacheRead = int64(v)
			}
		} else if num == 11 {
			respID = string(d)
		}
	})
	if err != nil || toks.Zero() {
		return call, 0, false
	}

	call.Timestamp = began
	call.Tokens = toks
	call.ResponseID = respID
	if !first.IsZero() && first.After(began) {
		call.TTFTMs = first.Sub(began).Milliseconds()
	}
	if !ended.IsZero() && ended.After(began) {
		call.DurationMs = ended.Sub(began).Milliseconds()
	}
	return call, genIdx, true
}

// agParseModel reads gen_metadata.data field 1 -> nested field 19, else 21, else 22.
func agParseModel(b []byte) string {
	var named [3]string
	_ = pbWalk(b, func(num int, v uint64, d []byte) {
		if num != 1 || d == nil {
			return
		}
		_ = pbWalk(d, func(n int, v uint64, d []byte) {
			if d == nil {
				return
			}
			switch n {
			case 19:
				named[0] = strings.TrimSpace(string(d))
			case 21:
				named[1] = strings.TrimSpace(string(d))
			case 22:
				named[2] = strings.TrimSpace(string(d))
			}
		})
	})
	for _, m := range named {
		if m != "" && len(m) <= 200 {
			return m
		}
	}
	return ""
}

func pbTime(b []byte) time.Time {
	var sec, nanos uint64
	_ = pbWalk(b, func(num int, v uint64, d []byte) {
		if d != nil {
			return
		}
		switch num {
		case 1:
			sec = v
		case 2:
			nanos = v
		}
	})
	if sec == 0 || sec > 1<<40 || nanos >= 1e9 {
		return time.Time{}
	}
	return time.Unix(int64(sec), int64(nanos))
}

func pbWalk(b []byte, field func(num int, v uint64, d []byte)) error {
	for len(b) > 0 {
		tag, n := binary.Uvarint(b)
		if n <= 0 || tag>>3 == 0 {
			return errPB
		}
		b = b[n:]
		num := int(tag >> 3)
		switch tag & 7 {
		case 0:
			v, n := binary.Uvarint(b)
			if n <= 0 {
				return errPB
			}
			b = b[n:]
			field(num, v, nil)
		case 1:
			if len(b) < 8 {
				return errPB
			}
			b = b[8:]
		case 2:
			l, n := binary.Uvarint(b)
			if n <= 0 || l > uint64(len(b)-n) {
				return errPB
			}
			field(num, 0, b[n:n+int(l)])
			b = b[n+int(l):]
		case 5:
			if len(b) < 4 {
				return errPB
			}
			b = b[4:]
		default:
			return errPB
		}
	}
	return nil
}

package scanner

import (
	"database/sql"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"sync"

	_ "modernc.org/sqlite"

	"github.com/Konakonai/lilya-tools/finch/internal/model"
)

type sourceFile struct {
	Path    string
	Source  string
	Size    int64
	MtimeNS int64
}

// Options configures where the scanner looks for logs and stores its SQLite cache.
type Options struct {
	HomeDir       string
	CacheDBPath   string
	IncludeMagpie bool
}

// DefaultOptions returns standard home and XDG cache paths with Magpie import enabled.
func DefaultOptions() Options {
	home, _ := os.UserHomeDir()
	cacheDir, err := os.UserCacheDir()
	if err != nil || cacheDir == "" {
		cacheDir = filepath.Join(home, ".cache")
	}
	return Options{
		HomeDir:       home,
		CacheDBPath:   filepath.Join(cacheDir, "finch", "index.db"),
		IncludeMagpie: true,
	}
}

// ScanSummary reports what the scanner found on the last run.
type ScanSummary struct {
	FilesTotal  int
	FilesParsed int
	FilesCached int
	CallsTotal  int
	MagpieDedup int
}

// Scanner discovers and parses local AI agent logs with an incremental SQLite cache.
type Scanner struct {
	opts Options
}

// New creates a Scanner.
func New(opts Options) *Scanner {
	if opts.HomeDir == "" {
		opts.HomeDir, _ = os.UserHomeDir()
	}
	return &Scanner{opts: opts}
}

// Scan scans all configured local agent log sources, using the SQLite cache for
// unchanged files, and returns deduplicated calls sorted newest-first.
func (s *Scanner) Scan() ([]model.Call, ScanSummary, error) {
	var files []sourceFile
	files = append(files, discoverClaudeFiles(s.opts.HomeDir)...)
	files = append(files, discoverCodexFiles(s.opts.HomeDir)...)
	files = append(files, discoverAntigravityFiles(s.opts.HomeDir)...)
	files = append(files, discoverPiFiles(s.opts.HomeDir)...)
	files = append(files, discoverGrokFiles(s.opts.HomeDir)...)
	if s.opts.IncludeMagpie {
		files = append(files, discoverMagpieFiles(s.opts.HomeDir)...)
	}

	var sum ScanSummary
	sum.FilesTotal = len(files)

	cache, _ := openFileCache(s.opts.CacheDBPath)
	if cache != nil {
		defer cache.Close()
	}

	type fileResult struct {
		file   sourceFile
		calls  []model.Call
		cached bool
	}

	results := make([]fileResult, len(files))
	activePaths := make(map[string]struct{}, len(files))
	var toParse []int

	for i, f := range files {
		activePaths[f.Path] = struct{}{}
		if cache != nil {
			if calls, ok := cache.get(f.Path, f.Size, f.MtimeNS); ok {
				results[i] = fileResult{file: f, calls: calls, cached: true}
				sum.FilesCached++
				continue
			}
		}
		toParse = append(toParse, i)
	}

	// Parse modified/new files concurrently.
	if len(toParse) > 0 {
		workers := min(max(runtime.NumCPU(), 2), 8)
		jobs := make(chan int, len(toParse))
		for _, idx := range toParse {
			jobs <- idx
		}
		close(jobs)

		var wg sync.WaitGroup
		for range workers {
			wg.Add(1)
			go func() {
				defer wg.Done()
				for idx := range jobs {
					f := files[idx]
					calls, _ := parseFile(f)
					results[idx] = fileResult{file: f, calls: calls}
				}
			}()
		}
		wg.Wait()
		sum.FilesParsed = len(toParse)

		if cache != nil {
			tx, err := cache.db.Begin()
			if err == nil {
				for _, idx := range toParse {
					r := results[idx]
					_ = cache.putTx(tx, r.file, r.calls)
				}
				_ = tx.Commit()
			}
		}
	}

	if cache != nil {
		cache.prune(activePaths)
	}

	var direct []model.Call
	var magpie []model.Call
	for _, r := range results {
		if r.file.Source == "magpie" {
			magpie = append(magpie, r.calls...)
		} else {
			direct = append(direct, r.calls...)
		}
	}

	merged, deduped := mergeAndDedupe(direct, magpie)
	sum.CallsTotal = len(merged)
	sum.MagpieDedup = deduped

	sort.Slice(merged, func(i, j int) bool {
		if !merged[i].Timestamp.Equal(merged[j].Timestamp) {
			return merged[i].Timestamp.After(merged[j].Timestamp)
		}
		return merged[i].ID < merged[j].ID
	})
	return merged, sum, nil
}

func parseFile(f sourceFile) ([]model.Call, error) {
	switch f.Source {
	case "claude":
		return parseClaudeFile(f.Path)
	case "codex":
		return parseCodexFile(f.Path)
	case "antigravity", "agy-cli":
		return parseAntigravityFile(f.Path, f.Source)
	case "pi":
		return parsePiFile(f.Path)
	case "grok":
		return parseGrokFile(f.Path)
	case "magpie":
		return parseMagpieFile(f.Path)
	default:
		return nil, nil
	}
}

type tokenSig struct {
	in, out, cr int64
}

func mergeAndDedupe(direct, magpie []model.Call) ([]model.Call, int) {
	if len(magpie) == 0 {
		return direct, 0
	}
	seenResp := make(map[string]struct{}, len(direct))
	bySig := make(map[tokenSig][]int64, len(direct))
	for _, c := range direct {
		if c.ResponseID != "" {
			seenResp[c.ResponseID] = struct{}{}
		}
		sig := tokenSig{in: c.Tokens.Input, out: c.Tokens.Output, cr: c.Tokens.CacheRead}
		bySig[sig] = append(bySig[sig], c.Timestamp.Unix())
	}

	out := make([]model.Call, 0, len(direct)+len(magpie))
	out = append(out, direct...)
	deduped := 0
	for _, m := range magpie {
		if m.ResponseID != "" {
			if _, ok := seenResp[m.ResponseID]; ok {
				deduped++
				continue
			}
		}
		sig := tokenSig{in: m.Tokens.Input, out: m.Tokens.Output, cr: m.Tokens.CacheRead}
		if times, ok := bySig[sig]; ok {
			mt := m.Timestamp.Unix()
			matched := false
			for _, dt := range times {
				diff := mt - dt
				if diff < 0 {
					diff = -diff
				}
				if diff <= 15 {
					matched = true
					break
				}
			}
			if matched {
				deduped++
				continue
			}
		}
		out = append(out, m)
	}
	return out, deduped
}

type fileCache struct {
	db *sql.DB
}

func openFileCache(path string) (*fileCache, error) {
	if path == "" {
		return nil, nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	_, err = db.Exec(`
		PRAGMA journal_mode=WAL;
		CREATE TABLE IF NOT EXISTS parsed_files (
			path TEXT PRIMARY KEY,
			source TEXT NOT NULL,
			size INTEGER NOT NULL,
			mtime_ns INTEGER NOT NULL,
			calls_json BLOB NOT NULL
		);
	`)
	if err != nil {
		db.Close()
		return nil, err
	}
	return &fileCache{db: db}, nil
}

func (fc *fileCache) Close() error {
	if fc == nil || fc.db == nil {
		return nil
	}
	return fc.db.Close()
}

func (fc *fileCache) get(path string, size, mtimeNS int64) ([]model.Call, bool) {
	var raw []byte
	err := fc.db.QueryRow(
		"SELECT calls_json FROM parsed_files WHERE path = ? AND size = ? AND mtime_ns = ?",
		path, size, mtimeNS,
	).Scan(&raw)
	if err != nil {
		return nil, false
	}
	var calls []model.Call
	if json.Unmarshal(raw, &calls) != nil {
		return nil, false
	}
	return calls, true
}

func (fc *fileCache) putTx(tx *sql.Tx, f sourceFile, calls []model.Call) error {
	raw, err := json.Marshal(calls)
	if err != nil {
		return err
	}
	_, err = tx.Exec(
		`INSERT OR REPLACE INTO parsed_files(path, source, size, mtime_ns, calls_json) VALUES (?, ?, ?, ?, ?)`,
		f.Path, f.Source, f.Size, f.MtimeNS, raw,
	)
	return err
}

func (fc *fileCache) prune(active map[string]struct{}) {
	rows, err := fc.db.Query("SELECT path FROM parsed_files")
	if err != nil {
		return
	}
	var stale []string
	for rows.Next() {
		var p string
		if rows.Scan(&p) == nil {
			if _, ok := active[p]; !ok {
				stale = append(stale, p)
			}
		}
	}
	rows.Close()
	if len(stale) == 0 {
		return
	}
	tx, err := fc.db.Begin()
	if err != nil {
		return
	}
	for _, p := range stale {
		_, _ = tx.Exec("DELETE FROM parsed_files WHERE path = ?", p)
	}
	_ = tx.Commit()
}

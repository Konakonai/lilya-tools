//! Scanning, caching and cross-file deduplication.
//!
//! Parsed files are cached in SQLite keyed by path, size and mtime, so a warm
//! start only re-reads what changed. `Engine::build` merges every file into a
//! [`Dataset`]: one entry per request, priced, sorted by time.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use rusqlite::{Connection, params};

use crate::model::{Agent, Call, FileParse, QuotaWindow, SessionMeta};
use crate::pricing::Catalog;
use crate::sources::{self, PARSER_VERSION, Source, SourceFile};

pub type SessionKey = (Agent, String);

#[derive(Clone, Debug, Default)]
pub struct ScanStats {
    #[allow(dead_code)] // shown by KESTREL_DEBUG through Debug
    pub files: usize,
    pub parsed: usize,
    pub calls: usize,
    pub duplicates: usize,
    pub gateway_merged: usize,
    pub millis: u128,
}

/// Immutable snapshot handed to the UI and reports.
#[derive(Default)]
pub struct Dataset {
    /// Sorted by start time.
    pub calls: Vec<Call>,
    pub sessions: HashMap<SessionKey, SessionMeta>,
    pub quotas: Vec<QuotaWindow>,
    pub stats: ScanStats,
}

impl Dataset {
    pub fn session(&self, agent: Agent, id: &str) -> Option<&SessionMeta> {
        self.sessions.get(&(agent, id.to_string()))
    }

    /// Index of the first call at or after `ts`.
    pub fn lower_bound(&self, ts: i64) -> usize {
        self.calls.partition_point(|c| c.ts < ts)
    }

    pub fn between(&self, start: i64, end: i64) -> &[Call] {
        let a = self.lower_bound(start);
        let b = self.lower_bound(end);
        &self.calls[a..b.max(a)]
    }
}

struct Entry {
    file: SourceFile,
    parse: Arc<FileParse>,
}

pub struct Engine {
    home: PathBuf,
    db: Option<Connection>,
    files: HashMap<PathBuf, Entry>,
    include_magpie: bool,
    index: Vec<SessionMeta>,
    index_at: Option<Instant>,
}

impl Engine {
    pub fn open(home: &Path, cache_dir: &Path, include_magpie: bool) -> Engine {
        let db = fs::create_dir_all(cache_dir).ok().and_then(|_| open_db(&cache_dir.join("files.sqlite")).ok());
        Engine { home: home.to_path_buf(), db, files: HashMap::new(), include_magpie, index: Vec::new(), index_at: None }
    }

    /// Rescans everything, reusing cached parses for unchanged files.
    pub fn full_scan(&mut self) -> ScanStats {
        let t0 = Instant::now();
        let found = sources::discover(&self.home, self.include_magpie);
        let cached = self.load_cache();
        let mut stats = ScanStats { files: found.len(), ..Default::default() };

        let results: Vec<(SourceFile, Arc<FileParse>, bool)> = found
            .into_par_iter()
            .map(|f| {
                let key = f.path.to_string_lossy().to_string();
                if let Some(entry) = self.files.get(&f.path)
                    && entry.file.size == f.size
                    && entry.file.mtime_ns == f.mtime_ns
                {
                    return (f, entry.parse.clone(), false);
                }
                if let Some((size, mtime, version, blob)) = cached.get(&key)
                    && *size == f.size
                    && *mtime == f.mtime_ns
                    && *version == PARSER_VERSION
                    && let Ok(p) = serde_json::from_slice::<FileParse>(blob)
                {
                    return (f, Arc::new(p), false);
                }
                let p = f.parse();
                (f, Arc::new(p), true)
            })
            .collect();

        let mut fresh = Vec::new();
        self.files.clear();
        for (f, p, parsed) in results {
            if parsed {
                stats.parsed += 1;
                fresh.push((f.clone(), p.clone()));
            }
            self.files.insert(f.path.clone(), Entry { file: f, parse: p });
        }
        self.store(&fresh);
        self.prune_cache();
        self.refresh_index(true);
        stats.millis = t0.elapsed().as_millis();
        stats
    }

    /// Re-reads the given paths (from the file watcher). Returns true if anything changed.
    pub fn update_paths(&mut self, paths: &[PathBuf]) -> bool {
        let roots = sources::roots(&self.home);
        let mut changed = Vec::new();
        let mut removed = false;
        let mut seen = HashSet::new();
        for p in paths {
            // Antigravity writes go to the WAL first.
            let p = match p.to_str().and_then(|s| s.strip_suffix("-wal").or_else(|| s.strip_suffix("-shm"))) {
                Some(db) => PathBuf::from(db),
                None => p.clone(),
            };
            // Grok's turn timings live next to updates.jsonl.
            let p = if p.file_name().is_some_and(|n| n == "events.jsonl") { p.with_file_name("updates.jsonl") } else { p };
            if !seen.insert(p.clone()) {
                continue;
            }
            let Some(source) = roots.iter().find(|(r, _)| p.starts_with(r)).map(|(_, s)| *s) else { continue };
            match sources::classify(&p, source) {
                Some(f) => {
                    let same = self.files.get(&p).is_some_and(|e| e.file.size == f.size && e.file.mtime_ns == f.mtime_ns);
                    if !same {
                        changed.push(f);
                    }
                }
                None => {
                    if self.files.remove(&p).is_some() {
                        removed = true;
                    }
                }
            }
        }
        if changed.is_empty() {
            return removed;
        }
        let parsed: Vec<(SourceFile, Arc<FileParse>)> = changed
            .into_par_iter()
            .map(|f| {
                let p = Arc::new(f.parse());
                (f, p)
            })
            .collect();
        for (f, p) in &parsed {
            self.files.insert(f.path.clone(), Entry { file: f.clone(), parse: p.clone() });
        }
        self.store(&parsed);
        self.refresh_index(false);
        true
    }

    fn refresh_index(&mut self, force: bool) {
        if !force && self.index_at.is_some_and(|t| t.elapsed().as_secs() < 60) {
            return;
        }
        let mut index = sources::antigravity::summaries(&self.home);
        index.extend(codex_titles(&self.home));
        self.index = index;
        self.index_at = Some(Instant::now());
    }

    /// Merges all files into a deduplicated, priced dataset.
    pub fn build(&self, catalog: &Catalog) -> Dataset {
        let t0 = Instant::now();
        let mut direct: Vec<&Call> = Vec::new();
        let mut gateway: Vec<&Call> = Vec::new();
        let mut sessions: HashMap<SessionKey, SessionMeta> = HashMap::new();
        let mut quotas: Vec<QuotaWindow> = Vec::new();
        let mut model_names: HashMap<&str, &str> = HashMap::new();

        for e in self.files.values() {
            for (placeholder, name) in &e.parse.model_names {
                model_names.insert(placeholder, name);
            }
            let into = if e.file.source == Source::Magpie { &mut gateway } else { &mut direct };
            into.extend(e.parse.calls.iter());
            for m in &e.parse.sessions {
                if let Some(agent) = m.agent {
                    merge_meta(sessions.entry((agent, m.id.clone())).or_default(), m);
                }
            }
            for q in &e.parse.quotas {
                match quotas.iter_mut().find(|o| o.agent == q.agent && o.label == q.label) {
                    Some(o) if o.observed_at >= q.observed_at => {}
                    Some(o) => *o = q.clone(),
                    None => quotas.push(q.clone()),
                }
            }
        }
        for m in &self.index {
            if let Some(agent) = m.agent {
                merge_meta(sessions.entry((agent, m.id.clone())).or_default(), m);
            }
        }

        // Forks copy history into a new session; keep each request in the
        // session that existed first.
        let mut first_seen: HashMap<(Agent, &str), i64> = HashMap::new();
        for c in &direct {
            let e = first_seen.entry((c.agent, c.session.as_str())).or_insert(c.ts);
            *e = (*e).min(c.ts);
        }
        let mut stats = ScanStats { files: self.files.len(), ..Default::default() };
        let mut by_key: HashMap<&str, usize> = HashMap::with_capacity(direct.len());
        let mut out: Vec<Call> = Vec::with_capacity(direct.len());
        for c in direct {
            match by_key.get(c.key.as_str()) {
                None => {
                    by_key.insert(c.key.as_str(), out.len());
                    out.push(c.clone());
                }
                Some(&i) => {
                    stats.duplicates += 1;
                    let cur = &out[i];
                    let a = first_seen.get(&(c.agent, c.session.as_str())).copied().unwrap_or(i64::MAX);
                    let b = first_seen.get(&(cur.agent, cur.session.as_str())).copied().unwrap_or(i64::MAX);
                    if a < b || (a == b && c.tokens.total() > cur.tokens.total()) {
                        out[i] = c.clone();
                    }
                }
            }
        }

        // Gateway rows: lend measured timing to the agent's own record, then drop.
        let mut by_response: HashMap<String, usize> = HashMap::new();
        let mut by_sig: HashMap<(u64, u64, u64), Vec<usize>> = HashMap::new();
        for (i, c) in out.iter().enumerate() {
            if let Some(r) = &c.response_id {
                by_response.insert(r.clone(), i);
            }
            by_sig.entry((c.tokens.input, c.tokens.output, c.tokens.cache_read)).or_default().push(i);
        }
        for g in gateway {
            let hit = g.response_id.as_ref().and_then(|r| by_response.get(r).copied()).or_else(|| {
                by_sig
                    .get(&(g.tokens.input, g.tokens.output, g.tokens.cache_read))?
                    .iter()
                    .copied()
                    .find(|&i| (out[i].ts - g.ts).abs() <= 15_000)
            });
            match hit {
                Some(i) => {
                    stats.gateway_merged += 1;
                    let c = &mut out[i];
                    if c.ttft_ms.is_none() && g.ttft_ms.is_some() {
                        c.ttft_ms = g.ttft_ms;
                        c.duration_ms = g.duration_ms;
                        c.decode = g.decode;
                    }
                }
                None => out.push(g.clone()),
            }
        }

        for c in &mut out {
            if c.model.starts_with(sources::antigravity::PLACEHOLDER_PREFIX)
                && let Some(name) = model_names.get(c.model.as_str())
            {
                c.model = name.to_string();
            }
            let (usd, kind, saved) = catalog.price(c);
            c.usd = usd;
            c.priced = kind;
            c.saved_usd = saved;
        }
        out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.key.cmp(&b.key)));

        for c in &out {
            sessions.entry((c.agent, c.session.clone())).or_insert_with(|| SessionMeta {
                agent: Some(c.agent),
                id: c.session.clone(),
                ..Default::default()
            });
        }
        stats.calls = out.len();
        stats.millis = t0.elapsed().as_millis();
        Dataset { calls: out, sessions, quotas, stats }
    }

    fn load_cache(&self) -> HashMap<String, (u64, i64, u32, Vec<u8>)> {
        let mut out = HashMap::new();
        let Some(db) = &self.db else { return out };
        let Ok(mut stmt) = db.prepare("SELECT path, size, mtime_ns, version, data FROM files") else { return out };
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)? as u32,
                r.get::<_, Vec<u8>>(4)?,
            ))
        });
        if let Ok(rows) = rows {
            for (p, s, m, v, d) in rows.flatten() {
                out.insert(p, (s, m, v, d));
            }
        }
        out
    }

    fn store(&mut self, items: &[(SourceFile, Arc<FileParse>)]) {
        let Some(db) = &mut self.db else { return };
        let Ok(tx) = db.transaction() else { return };
        for (f, p) in items {
            if let Ok(blob) = serde_json::to_vec(p.as_ref()) {
                let _ = tx.execute(
                    "INSERT OR REPLACE INTO files(path, source, size, mtime_ns, version, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![f.path.to_string_lossy(), f.source.name(), f.size as i64, f.mtime_ns, PARSER_VERSION, blob],
                );
            }
        }
        let _ = tx.commit();
    }

    fn prune_cache(&mut self) {
        let keep: HashSet<String> = self.files.keys().map(|p| p.to_string_lossy().to_string()).collect();
        let Some(db) = &mut self.db else { return };
        let stale: Vec<String> = {
            let Ok(mut stmt) = db.prepare("SELECT path FROM files") else { return };
            let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else { return };
            rows.flatten().filter(|p| !keep.contains(p)).collect()
        };
        if stale.is_empty() {
            return;
        }
        if let Ok(tx) = db.transaction() {
            for p in stale {
                let _ = tx.execute("DELETE FROM files WHERE path = ?1", [p]);
            }
            let _ = tx.commit();
        }
    }
}

fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS files (
             path TEXT PRIMARY KEY,
             source TEXT NOT NULL,
             size INTEGER NOT NULL,
             mtime_ns INTEGER NOT NULL,
             version INTEGER NOT NULL,
             data BLOB NOT NULL
         );",
    )?;
    Ok(db)
}

fn merge_meta(into: &mut SessionMeta, from: &SessionMeta) {
    if into.id.is_empty() {
        into.id.clone_from(&from.id);
    }
    if into.agent.is_none() {
        into.agent = from.agent;
    }
    if from.title.is_some() && (into.title.is_none() || from.agent == Some(Agent::Antigravity)) {
        into.title.clone_from(&from.title);
    }
    if into.cwd.is_none() {
        into.cwd.clone_from(&from.cwd);
    }
    if into.parent.is_none() {
        into.parent.clone_from(&from.parent);
    }
}

/// Thread names from `~/.codex/session_index.jsonl` (latest entry wins).
fn codex_titles(home: &Path) -> Vec<SessionMeta> {
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        thread_name: Option<String>,
    }
    let Ok(buf) = fs::read(home.join(".codex/session_index.jsonl")) else { return Vec::new() };
    let mut map: HashMap<String, String> = HashMap::new();
    for line in buf.split(|&b| b == b'\n') {
        if let Ok(r) = serde_json::from_slice::<Row>(line)
            && let Some(t) = r.thread_name.filter(|t| !t.trim().is_empty())
        {
            map.insert(r.id, t);
        }
    }
    map.into_iter().map(|(id, title)| SessionMeta { agent: Some(Agent::Codex), id, title: Some(title), cwd: None, parent: None }).collect()
}

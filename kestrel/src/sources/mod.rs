//! Log discovery and per-agent parsers.
//!
//! Every parser turns one file into a [`FileParse`]. Parsers are pure: they do
//! not deduplicate across files, which happens in `ingest`.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod grok;
pub mod magpie;
pub mod pi;
mod proto;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::model::FileParse;

/// Bump when any parser changes, so cached parses are rebuilt.
pub const PARSER_VERSION: u32 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Source {
    Claude,
    Codex,
    Antigravity,
    Pi,
    Grok,
    Magpie,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Claude => "claude",
            Source::Codex => "codex",
            Source::Antigravity => "antigravity",
            Source::Pi => "pi",
            Source::Grok => "grok",
            Source::Magpie => "magpie",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    pub source: Source,
    /// Size and mtime fingerprint (Antigravity folds in the WAL file).
    pub size: u64,
    pub mtime_ns: i64,
}

impl SourceFile {
    pub fn parse(&self) -> FileParse {
        let res = match self.source {
            Source::Claude => claude::parse(&self.path),
            Source::Codex => codex::parse(&self.path),
            Source::Antigravity => antigravity::parse(&self.path),
            Source::Pi => pi::parse(&self.path),
            Source::Grok => grok::parse(&self.path),
            Source::Magpie => magpie::parse(&self.path),
        };
        res.unwrap_or_default()
    }
}

/// Directories watched for live updates, with the source they belong to.
pub fn roots(home: &Path) -> Vec<(PathBuf, Source)> {
    let mut v = vec![
        (home.join(".claude/projects"), Source::Claude),
        (home.join(".codex/sessions"), Source::Codex),
        (home.join(".codex/archived_sessions"), Source::Codex),
        (home.join(".gemini/antigravity/conversations"), Source::Antigravity),
        (home.join(".gemini/antigravity-insiders/conversations"), Source::Antigravity),
        (home.join(".gemini/antigravity-cli/conversations"), Source::Antigravity),
        (home.join(".pi/agent/sessions"), Source::Pi),
        (home.join(".grok/sessions"), Source::Grok),
    ];
    // Claude Desktop's local agent mode keeps a private CLAUDE_CONFIG_DIR per workspace.
    let cowork = home.join(".config/Claude/local-agent-mode-sessions");
    for dir in walk_dirs(&cowork, 4) {
        let projects = dir.join(".claude/projects");
        if projects.is_dir() {
            v.push((projects, Source::Claude));
        }
    }
    v
}

fn walk_dirs(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if depth == 0 {
        return out;
    }
    let Ok(rd) = fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let p = e.path();
            out.push(p.clone());
            out.extend(walk_dirs(&p, depth - 1));
        }
    }
    out
}

/// Classifies a path under one of the roots, returning its fingerprint.
pub fn classify(path: &Path, source: Source) -> Option<SourceFile> {
    let name = path.file_name()?.to_str()?;
    let ok = match source {
        Source::Claude | Source::Pi => name.ends_with(".jsonl"),
        // Codex compresses older rollouts.
        Source::Codex => name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"),
        Source::Grok => name == "updates.jsonl",
        Source::Antigravity => name.ends_with(".db"),
        Source::Magpie => name == "usage.jsonl",
    };
    if !ok {
        return None;
    }
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let mut size = meta.len();
    let mut mtime_ns = mtime(&meta);
    if source == Source::Antigravity {
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        if let Ok(w) = fs::metadata(&wal) {
            size += w.len();
            mtime_ns = mtime_ns.max(mtime(&w));
        }
    }
    Some(SourceFile { path: path.to_path_buf(), source, size, mtime_ns })
}

fn mtime(meta: &fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// Finds every parseable file.
pub fn discover(home: &Path, include_magpie: bool) -> Vec<SourceFile> {
    let mut files = Vec::new();
    for (root, source) in roots(home) {
        collect(&root, source, &mut files, 6);
    }
    if include_magpie {
        for p in [home.join(".config/magpie/usage.jsonl"), home.join(".local/share/magpie/usage.jsonl")] {
            if let Some(f) = classify(&p, Source::Magpie) {
                files.push(f);
            }
        }
    }
    files
}

fn collect(dir: &Path, source: Source, out: &mut Vec<SourceFile>, depth: usize) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            if depth > 0 {
                collect(&p, source, out, depth - 1);
            }
        } else if let Some(f) = classify(&p, source) {
            out.push(f);
        }
    }
}

/// Streams the lines of a plain or zstd-compressed (`.zst`) file.
pub(crate) struct LineReader {
    inner: Box<dyn BufRead + Send>,
    buf: Vec<u8>,
}

impl LineReader {
    pub fn open(path: &Path) -> std::io::Result<LineReader> {
        let file = fs::File::open(path)?;
        let inner: Box<dyn BufRead + Send> = if path.extension().is_some_and(|e| e == "zst") {
            Box::new(BufReader::with_capacity(1 << 20, zstd::stream::read::Decoder::new(file)?))
        } else {
            Box::new(BufReader::with_capacity(1 << 20, file))
        };
        Ok(LineReader { inner, buf: Vec::with_capacity(1 << 16) })
    }

    /// The next non-empty line without its newline; None at end of file or on error.
    pub fn next_line(&mut self) -> Option<&[u8]> {
        loop {
            self.buf.clear();
            match self.inner.read_until(b'\n', &mut self.buf) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {
                    while matches!(self.buf.last(), Some(b'\n' | b'\r')) {
                        self.buf.pop();
                    }
                    if !self.buf.is_empty() {
                        return Some(&self.buf);
                    }
                }
            }
        }
    }
}

pub(crate) fn contains(hay: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(hay, needle).is_some()
}

/// The string value of the first `"key":"..."` in a raw JSON line, without
/// parsing the line. Only for machine-written logs with a stable field order.
pub(crate) fn raw_str<'a>(line: &'a [u8], key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":\"");
    let start = memchr::memmem::find(line, pat.as_bytes())? + pat.len();
    let end = start + memchr::memchr(b'"', &line[start..])?;
    std::str::from_utf8(&line[start..end]).ok()
}

/// Parses an RFC 3339 timestamp to unix milliseconds.
pub(crate) fn parse_ts(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

pub(crate) fn clamp_ms(v: i64) -> Option<u32> {
    (0..=30 * 60 * 1000).contains(&v).then_some(v as u32)
}

pub(crate) fn file_stem(path: &Path) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    let name = name.strip_suffix(".zst").unwrap_or(name);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains('-') => stem.to_string(),
        _ => name.to_string(),
    }
}

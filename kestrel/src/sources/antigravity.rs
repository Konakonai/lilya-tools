//! Antigravity conversations (`~/.gemini/antigravity*/conversations/<id>.db`).
//!
//! Each conversation is a SQLite database whose `steps.metadata` column holds an
//! undocumented protobuf (CortexStepMetadata). Field numbers were mapped by hand:
//!
//! ```text
//! 1  created_at            Timestamp   request sent
//! 6  viewable_at           Timestamp   first visible (non-thinking) token
//! 7  finished_generating   Timestamp
//! 9  usage                 { 1 model enum, 2 input (uncached), 3 output (= 9 + 10),
//!                            4 cache write, 5 cache read, 9 thinking, 10 visible,
//!                            11 response id }
//! 20 location              { 3 gen_metadata idx }
//! ```
//!
//! `gen_metadata.data` names the model: field 1.19 is the served model and
//! 3.28 the requested one, which carries the effort suffix (`gemini-4-neon-high`).
//!
//! Antigravity only timestamps the first *visible* token, after thinking. The
//! visible phase gives the decode rate, which also estimates how long the
//! thinking tokens took, so TTFT = viewable - created - thinking / rate.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::proto::{self, Field};
use super::{clamp_ms, file_stem};
use crate::model::{Agent, Call, FileParse, SessionMeta, Tokens};

struct Step {
    created: i64,
    viewable: Option<i64>,
    finished: Option<i64>,
    model_enum: u64,
    gen_idx: Option<i64>,
    tokens: Tokens,
    visible: u64,
    response_id: Option<String>,
}

fn parse_step(b: &[u8]) -> Option<Step> {
    let mut created = None;
    let mut viewable = None;
    let mut finished = None;
    let mut usage: Option<&[u8]> = None;
    let mut gen_idx = None;
    let ok = proto::walk(b, |n, v| {
        if let Field::Bytes(d) = v {
            match n {
                1 => created = proto::timestamp_ms(d),
                6 => viewable = proto::timestamp_ms(d),
                7 => finished = proto::timestamp_ms(d),
                9 => usage = Some(d),
                20 => {
                    proto::walk(d, |n, v| {
                        if let (3, Field::Varint(x)) = (n, v) {
                            gen_idx = Some(x as i64);
                        }
                    });
                }
                _ => {}
            }
        }
    });
    if !ok {
        return None;
    }
    let usage = usage?;
    let mut t = Tokens::default();
    let mut visible = 0;
    let mut model_enum = 0;
    let mut response_id = None;
    proto::walk(usage, |n, v| match v {
        Field::Varint(x) => match n {
            1 => model_enum = x,
            2 => t.input = x,
            3 => t.output = x,
            4 => t.cache_write = x,
            5 => t.cache_read = x,
            9 => t.reasoning = x,
            10 => visible = x,
            _ => {}
        },
        Field::Bytes(d) => {
            if n == 11 {
                response_id = std::str::from_utf8(d).ok().map(str::to_string);
            }
        }
    });
    if t.is_empty() {
        return None;
    }
    if visible == 0 && t.output >= t.reasoning {
        visible = t.output - t.reasoning;
    }
    Some(Step { created: created?, viewable, finished, model_enum, gen_idx, tokens: t, visible, response_id })
}

/// (served model, effort) from a gen_metadata blob.
fn model_name(b: &[u8]) -> Option<(String, Option<String>)> {
    let served = proto::bytes(b, 1).and_then(|d| proto::bytes(d, 19)).and_then(|s| std::str::from_utf8(s).ok());
    let requested = proto::bytes(b, 3).and_then(|d| proto::bytes(d, 28)).and_then(|s| std::str::from_utf8(s).ok());
    let clean = |s: &str| {
        let s = s.trim();
        (!s.is_empty() && s.len() < 120).then(|| s.to_string())
    };
    let served = served.and_then(clean);
    let requested = requested.and_then(clean);
    match (served, requested) {
        (Some(s), Some(r)) => {
            let effort = r.strip_prefix(&s).and_then(|rest| rest.strip_prefix('-')).map(str::to_string);
            Some((s, effort.filter(|e| is_effort(e))))
        }
        (Some(s), None) => Some((s, None)),
        (None, Some(r)) => {
            let (base, effort) = split_effort(&r);
            Some((base, effort))
        }
        (None, None) => None,
    }
}

fn is_effort(s: &str) -> bool {
    matches!(s, "minimal" | "low" | "medium" | "high" | "xhigh" | "max")
}

fn split_effort(name: &str) -> (String, Option<String>) {
    if let Some((base, last)) = name.rsplit_once('-')
        && is_effort(last)
    {
        return (base.to_string(), Some(last.to_string()));
    }
    (name.to_string(), None)
}

pub fn parse(path: &Path) -> anyhow::Result<FileParse> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    db.busy_timeout(std::time::Duration::from_millis(1500))?;
    let conversation = file_stem(path);

    let mut steps: Vec<(i64, Step)> = Vec::new();
    {
        let mut stmt = db.prepare("SELECT idx, metadata FROM steps WHERE metadata IS NOT NULL ORDER BY idx")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let idx: i64 = row.get(0)?;
            let blob = row.get_ref(1)?.as_blob().unwrap_or_default();
            if let Some(s) = parse_step(blob) {
                steps.push((idx, s));
            }
        }
    }

    // Resolve each model enum through one gen_metadata row; the blobs embed
    // whole prompts, so read as few as possible.
    let mut names: HashMap<u64, (String, Option<String>)> = HashMap::new();
    {
        let mut stmt = db.prepare("SELECT data FROM gen_metadata WHERE idx = ?1")?;
        let mut tried: HashMap<u64, u8> = HashMap::new();
        for (_, s) in &steps {
            if names.contains_key(&s.model_enum) {
                continue;
            }
            let Some(gi) = s.gen_idx else { continue };
            // Some steps point at gen_metadata rows that no longer exist.
            let attempts = tried.entry(s.model_enum).or_insert(0);
            if *attempts >= 24 {
                continue;
            }
            *attempts += 1;
            let blob: Option<Vec<u8>> = stmt.query_row([gi], |r| r.get(0)).optional()?.flatten();
            if let Some(n) = blob.as_deref().and_then(model_name) {
                names.insert(s.model_enum, n);
            }
        }
    }

    let mut out = FileParse { model_names: names.iter().map(|(e, (m, _))| (placeholder(*e), m.clone())).collect(), ..Default::default() };
    let mut by_response: HashMap<String, usize> = HashMap::new();
    for (idx, s) in steps {
        // Unresolved enums get a placeholder that `ingest` maps using other conversations.
        let (model, effort) = names.get(&s.model_enum).cloned().unwrap_or_else(|| (placeholder(s.model_enum), None));
        let duration = s.finished.and_then(|f| clamp_ms(f - s.created));
        let mut ttft = None;
        let mut decode = None;
        if let (Some(v), Some(f)) = (s.viewable, s.finished) {
            let visible_ms = f - v;
            if s.visible >= 16 && visible_ms >= 250 {
                let rate = s.visible as f64 / visible_ms as f64; // tokens per ms
                decode = Some((s.visible as u32, visible_ms as u32));
                let thinking_ms = (s.tokens.reasoning as f64 / rate) as i64;
                // A non-positive estimate means thinking ran faster than the
                // visible decode suggests; better unknown than a fake 0 ms.
                let est = v - s.created - thinking_ms;
                ttft = if est > 0 { clamp_ms(est) } else { None };
            } else if s.tokens.reasoning == 0 {
                ttft = clamp_ms(v - s.created);
            }
        }
        let call = Call {
            key: match &s.response_id {
                Some(r) => format!("antigravity:{r}"),
                None => format!("antigravity:{conversation}:{idx}"),
            },
            agent: Agent::Antigravity,
            ts: s.created,
            session: conversation.clone(),
            model,
            effort,
            tokens: s.tokens,
            ttft_ms: ttft,
            duration_ms: duration,
            decode,
            reported_usd: None,
            response_id: s.response_id.clone(),
            calls: 1,
            ..Default::default()
        };
        // Several steps can come from one model response and repeat its usage.
        if let Some(r) = s.response_id {
            if let Some(&pos) = by_response.get(&r) {
                let prev = &mut out.calls[pos];
                let start = prev.ts.min(call.ts);
                if call.tokens.total() >= prev.tokens.total() {
                    *prev = call;
                }
                prev.ts = start;
                continue;
            }
            by_response.insert(r, out.calls.len());
        }
        out.calls.push(call);
    }
    Ok(out)
}

pub const PLACEHOLDER_PREFIX: &str = "antigravity-model-";

fn placeholder(e: u64) -> String {
    format!("{PLACEHOLDER_PREFIX}{e}")
}

/// Titles, workspaces and parent links from Antigravity's summary index.
pub fn summaries(home: &Path) -> Vec<SessionMeta> {
    let mut out = Vec::new();
    for dir in ["antigravity", "antigravity-insiders", "antigravity-cli"] {
        let path = home.join(".gemini").join(dir).join("conversation_summaries.db");
        let Ok(db) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else {
            continue;
        };
        let _ = db.busy_timeout(std::time::Duration::from_millis(1000));
        let Ok(mut stmt) = db.prepare("SELECT conversation_id, title, workspace_uris, parent_conversation_id FROM conversation_summaries")
        else {
            continue;
        };
        let rows =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)));
        let Ok(rows) = rows else { continue };
        for (id, title, uris, parent) in rows.flatten() {
            let cwd = first_workspace(&uris);
            out.push(SessionMeta {
                agent: Some(Agent::Antigravity),
                id,
                title: (!title.trim().is_empty()).then_some(title),
                cwd,
                parent: (!parent.is_empty()).then_some(parent),
            });
        }
    }
    out
}

fn first_workspace(uris: &str) -> Option<String> {
    // Stored as a JSON array or a comma separated list of file:// URIs.
    let s = uris.trim().trim_start_matches('[').trim_end_matches(']');
    let first = s.split(',').next()?.trim().trim_matches('"');
    let path = first.strip_prefix("file://").unwrap_or(first);
    (!path.is_empty()).then(|| percent_decode(path))
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

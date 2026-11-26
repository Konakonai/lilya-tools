//! Claude Code session transcripts (`~/.claude/projects/**/*.jsonl`).
//!
//! Each content block of a response is written as its own `assistant` line when
//! the block finishes, all sharing `message.id`. Since Claude Code 2.1.2xx an
//! `api-request` line marks when the request was sent and assistant lines point
//! back to it through `requestRef`, which gives an exact request start. Older
//! transcripts fall back to the preceding `user` line.
//!
//! Time to first token is the first block's completion time minus how long
//! that block streamed (`thinkingDurationMs` / `toolInputStreamMs`); text-first
//! responses carry no streaming time, so their TTFT stays unknown.
//!
//! Forked and resumed sessions copy earlier messages, ids included, into the
//! new file; `ingest` deduplicates them by message id.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::{LineReader, clamp_ms, contains, file_stem, parse_ts};
use crate::model::{Agent, Call, FileParse, SessionMeta, Tokens, decode_window};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line<'a> {
    #[serde(borrow)]
    r#type: Option<&'a str>,
    #[serde(borrow)]
    timestamp: Option<&'a str>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "requestRef")]
    request_ref: Option<String>,
    cwd: Option<String>,
    effort: Option<String>,
    #[serde(rename = "thinkingDurationMs")]
    thinking_ms: Option<i64>,
    #[serde(rename = "toolInputStreamMs")]
    tool_stream_ms: Option<i64>,
    #[serde(rename = "customTitle")]
    custom_title: Option<String>,
    #[serde(rename = "agentName")]
    agent_name: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_creation: Option<CacheCreation>,
    output_tokens_details: Option<OutputDetails>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CacheCreation {
    ephemeral_5m_input_tokens: u64,
    ephemeral_1h_input_tokens: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OutputDetails {
    thinking_tokens: u64,
}

struct Pending {
    call: Call,
    start: Option<i64>,
    first_end: i64,
    last_end: i64,
    /// Streaming time of the first block, when the transcript records it.
    first_stream: Option<i64>,
}

pub fn parse(path: &Path) -> anyhow::Result<FileParse> {
    let mut rdr = LineReader::open(path)?;
    // Subagent transcripts live in `<session>/subagents/agent-*.jsonl`.
    let fallback_session = match path.parent() {
        Some(dir) if dir.file_name().is_some_and(|n| n == "subagents") => dir.parent().map(file_stem).unwrap_or_else(|| file_stem(path)),
        _ => file_stem(path),
    };

    let mut requests: HashMap<String, i64> = HashMap::new();
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut last_user_ts: Option<i64> = None;
    let mut meta: HashMap<String, SessionMeta> = HashMap::new();
    let mut cwd: Option<String> = None;

    while let Some(raw) = rdr.next_line() {
        let is_assistant = contains(raw, b"\"usage\"");
        let is_request = !is_assistant && contains(raw, b"\"type\":\"api-request\"");
        let is_user = !is_assistant && !is_request && contains(raw, b"\"type\":\"user\"");
        let is_title = contains(raw, b"\"type\":\"custom-title\"") || contains(raw, b"\"type\":\"agent-name\"");
        if !(is_assistant || is_request || is_user || is_title) {
            continue;
        }
        let Ok(ln) = serde_json::from_slice::<Line>(raw) else { continue };
        let ts = ln.timestamp.and_then(parse_ts);
        if ln.cwd.is_some() {
            cwd.clone_from(&ln.cwd);
        }
        match ln.r#type {
            Some("api-request") => {
                if let (Some(id), Some(ts)) = (ln.id, ts) {
                    requests.insert(id, ts);
                }
                continue;
            }
            Some("user") => {
                if ts.is_some() {
                    last_user_ts = ts;
                }
                continue;
            }
            Some("custom-title") | Some("agent-name") => {
                let sid = ln.session_id.clone().unwrap_or_else(|| fallback_session.clone());
                let entry =
                    meta.entry(sid.clone()).or_insert_with(|| SessionMeta { agent: Some(Agent::Claude), id: sid, ..Default::default() });
                // A custom title wins over the auto-generated agent name.
                if let Some(t) = ln.custom_title.filter(|t| !t.trim().is_empty()) {
                    entry.title = Some(t);
                } else if entry.title.is_none() {
                    entry.title = ln.agent_name.filter(|t| !t.trim().is_empty());
                }
                continue;
            }
            Some("assistant") => {}
            _ => continue,
        }

        let Some(ts) = ts else { continue };
        let Some(msg) = ln.message else { continue };
        let Some(u) = msg.usage else { continue };
        let model = msg.model.unwrap_or_default();
        if model.is_empty() || model.starts_with('<') {
            continue; // "<synthetic>" placeholder messages
        }
        let (c5, c1h) = match &u.cache_creation {
            Some(cc) if cc.ephemeral_5m_input_tokens + cc.ephemeral_1h_input_tokens > 0 => {
                (cc.ephemeral_5m_input_tokens, cc.ephemeral_1h_input_tokens)
            }
            _ => (u.cache_creation_input_tokens, 0),
        };
        let tokens = Tokens {
            input: u.input_tokens,
            output: u.output_tokens,
            cache_read: u.cache_read_input_tokens,
            cache_write: c5,
            cache_write_1h: c1h,
            reasoning: u.output_tokens_details.map(|d| d.thinking_tokens).unwrap_or(0),
        };
        if tokens.is_empty() {
            continue;
        }
        let key = msg.id.clone().or_else(|| ln.request_id.clone()).unwrap_or_else(|| format!("{fallback_session}:{ts}"));
        let session = ln.session_id.unwrap_or_else(|| fallback_session.clone());
        let start = ln.request_ref.as_ref().and_then(|r| requests.get(r).copied()).or(last_user_ts).filter(|s| *s <= ts);
        let stream = ln.thinking_ms.or(ln.tool_stream_ms);

        match pending.get_mut(&key) {
            Some(p) => {
                // Later blocks carry the final usage; keep the largest.
                if tokens.output >= p.call.tokens.output && tokens.total() >= p.call.tokens.total() {
                    p.call.tokens = tokens;
                }
                p.last_end = p.last_end.max(ts);
                if ts < p.first_end {
                    p.first_end = ts;
                    p.first_stream = stream;
                }
            }
            None => {
                order.push(key.clone());
                pending.insert(
                    key.clone(),
                    Pending {
                        call: Call {
                            key: format!("claude:{key}"),
                            agent: Agent::Claude,
                            ts: start.unwrap_or(ts),
                            session,
                            model,
                            effort: ln.effort,
                            tokens,
                            ttft_ms: None,
                            duration_ms: None,
                            decode: None,
                            reported_usd: None,
                            response_id: msg.id,
                            calls: 1,
                            ..Default::default()
                        },
                        start,
                        first_end: ts,
                        last_end: ts,
                        first_stream: stream,
                    },
                );
            }
        }
    }

    let mut out = FileParse::default();
    for key in order {
        let Some(mut p) = pending.remove(&key) else { continue };
        if let Some(start) = p.start {
            p.call.duration_ms = clamp_ms(p.last_end - start);
            if let Some(stream) = p.first_stream {
                let ttft = p.first_end - stream - start;
                if ttft >= 0 {
                    p.call.ttft_ms = clamp_ms(ttft);
                }
            }
            p.call.decode = decode_window(p.call.tokens.output, p.call.duration_ms, p.call.ttft_ms);
        }
        out.calls.push(p.call);
    }

    if let Some(cwd) = cwd {
        for c in &out.calls {
            meta.entry(c.session.clone()).or_insert_with(|| SessionMeta {
                agent: Some(Agent::Claude),
                id: c.session.clone(),
                ..Default::default()
            });
        }
        for m in meta.values_mut() {
            m.cwd.get_or_insert_with(|| cwd.clone());
        }
    }
    out.sessions = meta.into_values().collect();
    Ok(out)
}

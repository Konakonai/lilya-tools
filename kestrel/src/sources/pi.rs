//! Pi coding agent sessions (`~/.pi/agent/sessions/<cwd>/<ts>_<id>.jsonl`).

use std::path::Path;

use serde::Deserialize;

use super::{LineReader, clamp_ms, contains, file_stem, parse_ts};
use crate::model::{Agent, Call, FileParse, SessionMeta, Tokens};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
    r#type: String,
    id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
    role: String,
    model: Option<String>,
    #[serde(rename = "responseModel")]
    response_model: Option<String>,
    #[serde(rename = "responseId")]
    response_id: Option<String>,
    #[serde(rename = "durationMs")]
    duration_ms: Option<i64>,
    #[serde(rename = "thinkingLevel")]
    thinking_level: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input: u64,
    output: u64,
    #[serde(rename = "cacheRead")]
    cache_read: u64,
    #[serde(rename = "cacheWrite")]
    cache_write: u64,
    reasoning: u64,
    cost: Option<Cost>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Cost {
    total: f64,
}

pub fn parse(path: &Path) -> anyhow::Result<FileParse> {
    let mut rdr = LineReader::open(path)?;
    let stem = file_stem(path);
    let mut session = stem.rsplit('_').next().unwrap_or(&stem).to_string();
    let mut cwd = None;
    let mut out = FileParse::default();
    while let Some(raw) = rdr.next_line() {
        let header = contains(raw, b"\"type\":\"session\"");
        if !header && !contains(raw, b"\"usage\"") {
            continue;
        }
        let Ok(ln) = serde_json::from_slice::<Line>(raw) else { continue };
        if ln.r#type == "session" {
            if let Some(id) = ln.id {
                session = id;
            }
            cwd = ln.cwd;
            continue;
        }
        let Some(msg) = ln.message else { continue };
        if msg.role != "assistant" {
            continue;
        }
        let Some(u) = msg.usage else { continue };
        let tokens = Tokens {
            input: u.input,
            output: u.output,
            cache_read: u.cache_read,
            cache_write: u.cache_write,
            cache_write_1h: 0,
            reasoning: u.reasoning,
        };
        if tokens.is_empty() {
            continue;
        }
        let Some(ts) = ln.timestamp.as_deref().and_then(parse_ts) else { continue };
        let duration = msg.duration_ms.and_then(clamp_ms);
        // Pi logs when the response ended; its duration recovers the start.
        let start = ts - duration.unwrap_or(0) as i64;
        let model = msg
            .response_model
            .filter(|m| !m.is_empty())
            .or(msg.model.map(|m| m.rsplit('/').next().unwrap_or(&m).to_string()))
            .unwrap_or_else(|| "pi".into());
        let key = match (&msg.response_id, &ln.id) {
            (Some(r), _) => format!("pi:{r}"),
            (None, Some(id)) => format!("pi:{session}:{id}"),
            (None, None) => format!("pi:{session}:{ts}"),
        };
        out.calls.push(Call {
            key,
            agent: Agent::Pi,
            ts: start,
            session: session.clone(),
            model,
            effort: msg.thinking_level.filter(|l| l != "off"),
            tokens,
            ttft_ms: None,
            duration_ms: duration,
            // No TTFT, so the decode window includes prefill: a lower bound on speed.
            decode: duration.map(|d| (u.output.min(u32::MAX as u64) as u32, d)),
            reported_usd: u.cost.map(|c| c.total).filter(|c| *c > 0.0),
            response_id: msg.response_id,
            calls: 1,
            ..Default::default()
        });
    }
    out.sessions.push(SessionMeta { agent: Some(Agent::Pi), id: session, title: None, cwd, parent: None });
    Ok(out)
}

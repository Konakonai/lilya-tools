//! Historical Magpie gateway log (`~/.config/magpie/usage.jsonl`).
//!
//! Magpie proxied several agents. Its rows overlap with the agents' own logs,
//! so `ingest` drops rows whose response id another source already has, after
//! lending them the gateway-measured timing.

use std::path::Path;

use serde::Deserialize;

use super::{LineReader, clamp_ms, parse_ts};
use crate::model::{Agent, Call, FileParse, Tokens, decode_window};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Row {
    t: String,
    agent: String,
    model: String,
    served: String,
    #[serde(rename = "in")]
    input: u64,
    out: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
    ms: i64,
    ttft_ms: i64,
    status: i64,
    kind: Option<String>,
    response_id: Option<String>,
    session: Option<String>,
    effort: Option<String>,
}

pub fn parse(path: &Path) -> anyhow::Result<FileParse> {
    let mut rdr = LineReader::open(path)?;
    let mut out = FileParse::default();
    let mut n = 0usize;
    while let Some(raw) = rdr.next_line() {
        n += 1;
        let Ok(r) = serde_json::from_slice::<Row>(raw) else { continue };
        if r.status >= 400 || r.status == 0 || r.kind.as_deref().is_some_and(|k| k == "model_list") {
            continue;
        }
        let tokens = Tokens {
            input: r.input,
            output: r.out,
            cache_read: r.cache_read,
            cache_write: r.cache_write,
            cache_write_1h: 0,
            reasoning: r.reasoning,
        };
        if tokens.is_empty() {
            continue;
        }
        let model = if r.served.trim().is_empty() { r.model.trim() } else { r.served.trim() };
        if model.is_empty() {
            continue;
        }
        let Some(end) = parse_ts(&r.t) else { continue };
        let duration = clamp_ms(r.ms);
        let ttft = (r.ttft_ms > 0).then(|| clamp_ms(r.ttft_ms)).flatten();
        let agent = match r.agent.as_str() {
            "claude" | "claude-code" | "claude-desktop" => Agent::Claude,
            "codex" | "codex-desktop" => Agent::Codex,
            "antigravity" | "agy" => Agent::Antigravity,
            "pi" => Agent::Pi,
            "grok" | "grok-build" => Agent::Grok,
            _ => Agent::Magpie,
        };
        out.calls.push(Call {
            key: match &r.response_id {
                Some(id) if !id.is_empty() => format!("magpie:{id}"),
                _ => format!("magpie:{n}"),
            },
            agent,
            ts: end - r.ms.max(0),
            session: r.session.filter(|s| !s.is_empty()).unwrap_or_else(|| format!("magpie-{}", r.agent)),
            model: model.rsplit('/').next().unwrap_or(model).to_string(),
            effort: r.effort.filter(|e| !e.is_empty()),
            tokens,
            ttft_ms: ttft,
            duration_ms: duration,
            decode: decode_window(r.out, duration, ttft),
            reported_usd: None,
            response_id: r.response_id.filter(|s| !s.is_empty()),
            calls: 1,
            ..Default::default()
        });
    }
    Ok(out)
}

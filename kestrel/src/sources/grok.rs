//! Grok CLI sessions (`~/.grok/sessions/<cwd>/<id>/updates.jsonl`).
//!
//! Usage is reported once per turn (`turn_completed`), split by model, and may
//! fold several model calls together. The sibling `events.jsonl` timestamps
//! each model loop and its first token, which gives TTFT.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::{LineReader, antigravity::percent_decode, clamp_ms, contains, parse_ts};
use crate::model::{Agent, Call, FileParse, SessionMeta, Tokens};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
    timestamp: f64,
    params: Params,
    #[serde(rename = "_meta")]
    meta: Option<Meta>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Meta {
    #[serde(rename = "agentTimestampMs")]
    agent_ts: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Params {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    update: Update,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Update {
    #[serde(rename = "sessionUpdate")]
    kind: String,
    prompt_id: Option<String>,
    usage: Option<Usage>,
    elapsed_ms: Option<i64>,
    #[serde(rename = "_meta")]
    meta: Option<Meta>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Usage {
    #[serde(rename = "inputTokens")]
    input: u64,
    #[serde(rename = "outputTokens")]
    output: u64,
    #[serde(rename = "cachedReadTokens")]
    cache_read: u64,
    #[serde(rename = "cacheCreationTokens")]
    cache_write: u64,
    #[serde(rename = "reasoningTokens")]
    reasoning: u64,
    #[serde(rename = "modelCalls")]
    model_calls: u32,
    #[serde(rename = "apiDurationMs")]
    api_ms: i64,
    #[serde(rename = "costUsdTicks")]
    ticks: i64,
    #[serde(rename = "modelUsage")]
    model_usage: BTreeMap<String, Usage>,
}

impl Usage {
    fn tokens(&self) -> Tokens {
        Tokens {
            input: self.input.saturating_sub(self.cache_read + self.cache_write),
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            cache_write_1h: 0,
            reasoning: self.reasoning,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Summary {
    info: SummaryInfo,
    session_summary: Option<String>,
    created_at: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SummaryInfo {
    cwd: Option<String>,
    current_model_id: Option<String>,
    parent_session_id: Option<String>,
    created_at: Option<String>,
}

/// One turn from events.jsonl: start time and summed time-to-first-token.
struct TurnTiming {
    start: i64,
    first_ttft: Option<i64>,
    ttft_sum: i64,
}

fn turn_timings(path: &Path) -> Vec<TurnTiming> {
    #[derive(Deserialize)]
    struct Ev<'a> {
        #[serde(borrow)]
        ts: &'a str,
        #[serde(borrow)]
        r#type: &'a str,
    }
    let Ok(mut rdr) = LineReader::open(path) else { return Vec::new() };
    let mut out: Vec<TurnTiming> = Vec::new();
    let mut loop_start: Option<i64> = None;
    while let Some(raw) = rdr.next_line() {
        let Ok(ev) = serde_json::from_slice::<Ev>(raw) else { continue };
        let Some(ts) = parse_ts(ev.ts) else { continue };
        match ev.r#type {
            "turn_started" => out.push(TurnTiming { start: ts, first_ttft: None, ttft_sum: 0 }),
            "loop_started" => loop_start = Some(ts),
            "first_token" => {
                if let (Some(ls), Some(t)) = (loop_start.take(), out.last_mut()) {
                    let ttft = ts - ls;
                    if (0..600_000).contains(&ttft) {
                        t.first_ttft.get_or_insert(ttft);
                        t.ttft_sum += ttft;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn parse(path: &Path) -> anyhow::Result<FileParse> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut session = dir.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    let cwd_from_dir = dir.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()).map(percent_decode);

    let summary: Summary = fs::read(dir.join("summary.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    // Child sessions replay their parent's history; count only their own turns.
    let since = summary
        .info
        .parent_session_id
        .as_ref()
        .and(summary.info.created_at.as_deref().or(summary.created_at.as_deref()))
        .and_then(parse_ts)
        .map(|t| t - t % 1000);
    let timings = turn_timings(&dir.join("events.jsonl"));

    let mut rdr = LineReader::open(path)?;
    let mut out = FileParse::default();
    while let Some(raw) = rdr.next_line() {
        if !contains(raw, b"\"turn_completed\"") {
            continue;
        }
        let Ok(ln) = serde_json::from_slice::<Line>(raw) else { continue };
        let up = ln.params.update;
        if up.kind != "turn_completed" {
            continue;
        }
        let Some(usage) = up.usage else { continue };
        let end = up
            .meta
            .as_ref()
            .and_then(|m| m.agent_ts)
            .or(ln.meta.as_ref().and_then(|m| m.agent_ts))
            .unwrap_or((ln.timestamp * 1000.0) as i64);
        if end <= 0 || since.is_some_and(|s| end < s) {
            continue;
        }
        if let Some(s) = ln.params.session_id {
            session = s;
        }
        let elapsed = up.elapsed_ms.unwrap_or(usage.api_ms).max(0);
        let start = end - elapsed;
        let timing = timings.iter().find(|t| (t.start - start).abs() < 3000);
        let ttft = timing.and_then(|t| t.first_ttft).and_then(clamp_ms);

        let per_model: Vec<(String, Usage)> = if usage.model_usage.is_empty() {
            let m = summary.info.current_model_id.clone().unwrap_or_else(|| "grok".into());
            vec![(m, usage.clone())]
        } else {
            usage.model_usage.clone().into_iter().collect()
        };
        let single = per_model.len() == 1;
        for (model, mu) in per_model {
            let tokens = mu.tokens();
            if tokens.is_empty() {
                continue;
            }
            let ticks = if mu.ticks == 0 && single { usage.ticks } else { mu.ticks };
            let api_ms = if mu.api_ms > 0 { mu.api_ms } else { usage.api_ms };
            let ttft_sum = timing.map(|t| t.ttft_sum).unwrap_or(0);
            let decode =
                (single && ttft_sum > 0 && api_ms > ttft_sum).then(|| (mu.output.min(u32::MAX as u64) as u32, (api_ms - ttft_sum) as u32));
            out.calls.push(Call {
                key: match &up.prompt_id {
                    Some(p) => format!("grok:{p}:{model}"),
                    None => format!("grok:{session}:{end}:{model}"),
                },
                agent: Agent::Grok,
                ts: start,
                session: session.clone(),
                model,
                effort: None,
                tokens,
                ttft_ms: ttft,
                duration_ms: clamp_ms(api_ms),
                decode,
                reported_usd: (ticks > 0).then(|| ticks as f64 / 1e10),
                response_id: None,
                calls: mu.model_calls.max(1),
                ..Default::default()
            });
        }
    }
    out.sessions.push(SessionMeta {
        agent: Some(Agent::Grok),
        id: session,
        title: summary.session_summary.filter(|s| !s.trim().is_empty()),
        cwd: summary.info.cwd.or(cwd_from_dir),
        parent: summary.info.parent_session_id.filter(|p| !p.is_empty()),
    });
    Ok(out)
}

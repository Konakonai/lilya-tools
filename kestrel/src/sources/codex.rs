//! Codex rollouts (`~/.codex/{sessions,archived_sessions}/**/*.jsonl`).
//!
//! Newer rollouts write one `token_usage_record` per model response, with the
//! upstream `response_id`. Older ones only have cumulative `token_count`
//! events, from which per-response deltas are derived. Responses are written
//! when complete, so a response's duration runs from the last input boundary
//! (tool output, user message, turn start) to its usage record. The turn's
//! `task_complete` event carries the true time to first token, which belongs to
//! the turn's first response.
//!
//! `token_count` events also carry the account's rate-limit windows.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::{LineReader, clamp_ms, file_stem, parse_ts, raw_str};
use crate::model::{Agent, Call, FileParse, QuotaWindow, SessionMeta, Tokens, decode_window};

#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
}

impl Usage {
    fn tokens(&self) -> Tokens {
        Tokens {
            input: self.input_tokens.saturating_sub(self.cached_input_tokens + self.cache_write_input_tokens),
            output: self.output_tokens,
            cache_read: self.cached_input_tokens,
            cache_write: self.cache_write_input_tokens,
            cache_write_1h: 0,
            reasoning: self.reasoning_output_tokens,
        }
    }

    fn minus(&self, prev: &Usage) -> Option<Usage> {
        if self.input_tokens < prev.input_tokens || self.output_tokens < prev.output_tokens {
            return None;
        }
        Some(Usage {
            input_tokens: self.input_tokens - prev.input_tokens,
            cached_input_tokens: self.cached_input_tokens.saturating_sub(prev.cached_input_tokens),
            cache_write_input_tokens: self.cache_write_input_tokens.saturating_sub(prev.cache_write_input_tokens),
            output_tokens: self.output_tokens - prev.output_tokens,
            reasoning_output_tokens: self.reasoning_output_tokens.saturating_sub(prev.reasoning_output_tokens),
        })
    }
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(borrow)]
    timestamp: Option<&'a str>,
    #[serde(borrow)]
    r#type: Option<&'a str>,
    #[serde(default)]
    payload: Value,
}

pub fn parse(path: &Path) -> anyhow::Result<FileParse> {
    let mut rdr = LineReader::open(path)?;
    // Rollout files are named rollout-<date>-<uuid>; session_meta normally overrides this.
    let stem = file_stem(path);
    let mut session = match stem.len().checked_sub(36) {
        Some(i) if stem.is_char_boundary(i) && stem[i..].matches('-').count() == 4 => stem[i..].to_string(),
        _ => stem,
    };
    let mut cwd: Option<String> = None;
    let mut model = String::new();
    let mut effort: Option<String> = None;
    let mut turn_id: Option<String> = None;

    let mut boundary: Option<i64> = None;
    let mut resp_start: Option<i64> = None;

    let mut records: Vec<Call> = Vec::new();
    let mut events: Vec<Call> = Vec::new();
    let mut first_of_turn: HashMap<String, (bool, usize)> = HashMap::new(); // turn -> (from records?, index)
    let mut prev_total: Option<Usage> = None;
    let mut quotas: Vec<QuotaWindow> = Vec::new();
    let mut plan: Option<String> = None;

    while let Some(raw) = rdr.next_line() {
        // Tool output lines dominate the file; classify them without parsing.
        let kind = raw_str(raw, "type").unwrap_or("");
        if kind == "response_item" {
            let Some(ts) = raw_str(raw, "timestamp").and_then(parse_ts) else { continue };
            let payload = memchr::memmem::find(raw, b"\"payload\":").map(|i| &raw[i..]).unwrap_or_default();
            match raw_str(payload, "type").unwrap_or("") {
                "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                    boundary = Some(ts);
                    resp_start = None;
                }
                "message" if raw_str(payload, "role").is_some_and(|r| r != "assistant") => {
                    boundary = Some(ts);
                    resp_start = None;
                }
                _ => {
                    if resp_start.is_none() {
                        resp_start = boundary;
                    }
                }
            }
            continue;
        }
        if kind == "event_msg" {
            let payload = memchr::memmem::find(raw, b"\"payload\":").map(|i| &raw[i..]).unwrap_or_default();
            if !matches!(raw_str(payload, "type"), Some("token_count" | "task_started" | "user_message" | "task_complete")) {
                continue;
            }
        } else if !matches!(kind, "session_meta" | "turn_context" | "token_usage_record") {
            continue;
        }
        let Ok(ln) = serde_json::from_slice::<Line>(raw) else { continue };
        let Some(ts) = ln.timestamp.and_then(parse_ts) else { continue };
        let p = &ln.payload;
        let ptype = p.get("type").and_then(Value::as_str).unwrap_or("");
        match ln.r#type.unwrap_or("") {
            "session_meta" => {
                if let Some(id) = p.get("session_id").or_else(|| p.get("id")).and_then(Value::as_str) {
                    session = id.to_string();
                }
                if let Some(c) = p.get("cwd").and_then(Value::as_str) {
                    cwd = Some(c.to_string());
                }
                if let Some(m) = p.get("model").and_then(Value::as_str) {
                    model = m.to_string();
                }
            }
            "turn_context" => {
                boundary = Some(ts);
                resp_start = None;
                if let Some(c) = p.get("cwd").and_then(Value::as_str) {
                    cwd = Some(c.to_string());
                }
                let settings = p.pointer("/collaboration_mode/settings");
                if let Some(m) =
                    p.get("model").and_then(Value::as_str).or_else(|| settings.and_then(|s| s.get("model")).and_then(Value::as_str))
                {
                    model = m.to_string();
                }
                if let Some(e) = p.get("effort").or_else(|| settings.and_then(|s| s.get("reasoning_effort"))).and_then(Value::as_str) {
                    effort = Some(e.to_string());
                }
                turn_id = p.get("turn_id").and_then(Value::as_str).map(str::to_string);
            }
            "token_usage_record" => {
                let Some(u) = p.get("usage").and_then(|u| Usage::deserialize(u).ok()) else { continue };
                let tokens = u.tokens();
                if tokens.is_empty() {
                    continue;
                }
                let start = resp_start.or(boundary).filter(|s| *s <= ts);
                let rid = p.get("response_id").and_then(Value::as_str).map(str::to_string);
                let tid = p.get("turn_id").and_then(Value::as_str).map(str::to_string).or_else(|| turn_id.clone());
                let m = p.get("model").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| model.clone());
                let key = match &rid {
                    Some(r) => format!("codex:{r}"),
                    None => format!("codex:{session}:r{}", records.len()),
                };
                if let Some(t) = tid {
                    first_of_turn.entry(t).or_insert((true, records.len()));
                }
                records.push(Call {
                    key,
                    agent: Agent::Codex,
                    ts: start.unwrap_or(ts),
                    session: session.clone(),
                    model: if m.is_empty() { "codex".into() } else { m },
                    effort: effort.clone(),
                    tokens,
                    ttft_ms: None,
                    duration_ms: start.and_then(|s| clamp_ms(ts - s)),
                    decode: None,
                    reported_usd: None,
                    response_id: rid,
                    calls: 1,
                    ..Default::default()
                });
                resp_start = None;
                boundary = Some(ts);
            }
            "event_msg" => match ptype {
                "token_count" => {
                    if let Some(rl) = p.get("rate_limits") {
                        if let Some(pt) = rl.get("plan_type").and_then(Value::as_str) {
                            plan = Some(pt.to_string());
                        }
                        let limit = rl.get("limit_id").and_then(Value::as_str).unwrap_or("codex");
                        for key in ["primary", "secondary"] {
                            if let Some(w) = rl.get(key).filter(|w| w.is_object())
                                && let Some(q) = quota(w, ts, limit, plan.clone())
                            {
                                quotas.retain(|o| o.window_minutes != q.window_minutes || o.label != q.label);
                                quotas.push(q);
                            }
                        }
                    }
                    let info = p.get("info");
                    let Some(info) = info.filter(|i| i.is_object()) else { continue };
                    if let Some(m) = info.get("model").and_then(Value::as_str) {
                        model = m.to_string();
                    }
                    let total = info.get("total_token_usage").and_then(|u| Usage::deserialize(u).ok());
                    let last = info.get("last_token_usage").and_then(|u| Usage::deserialize(u).ok());
                    let delta = match (total, prev_total) {
                        (Some(t), Some(pt)) if t == pt => None,
                        (Some(t), Some(pt)) => t.minus(&pt).or(last),
                        (Some(t), None) => last.or(Some(t)),
                        (None, _) => last,
                    };
                    if total.is_some() {
                        prev_total = total;
                    }
                    let Some(d) = delta else { continue };
                    let tokens = d.tokens();
                    if tokens.is_empty() {
                        continue;
                    }
                    if let Some(t) = &turn_id {
                        first_of_turn.entry(t.clone()).or_insert((false, events.len()));
                    }
                    events.push(Call {
                        key: format!("codex:{session}:e{}", events.len()),
                        agent: Agent::Codex,
                        ts,
                        session: session.clone(),
                        model: if model.is_empty() { "codex".into() } else { model.clone() },
                        effort: effort.clone(),
                        tokens,
                        ttft_ms: None,
                        duration_ms: None,
                        decode: None,
                        reported_usd: None,
                        response_id: None,
                        calls: 1,
                        ..Default::default()
                    });
                }
                "task_started" | "user_message" => {
                    boundary = Some(ts);
                    resp_start = None;
                }
                "task_complete" => {
                    let ttft = p.get("time_to_first_token_ms").and_then(Value::as_i64);
                    let tid = p.get("turn_id").and_then(Value::as_str);
                    if let (Some(ttft), Some(tid)) = (ttft, tid)
                        && let Some(&(from_records, idx)) = first_of_turn.get(tid)
                    {
                        let list = if from_records { &mut records } else { &mut events };
                        if let Some(c) = list.get_mut(idx) {
                            c.ttft_ms = clamp_ms(ttft);
                            if let Some(d) = c.duration_ms {
                                // The recorded start can trail the real one; never let TTFT exceed it.
                                c.duration_ms = Some(d.max(ttft as u32));
                            }
                            c.decode = decode_window(c.tokens.output, c.duration_ms, c.ttft_ms);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    // Newer rollouts carry both; records have exact response ids.
    let calls = if records.is_empty() { events } else { records };
    let mut out = FileParse { calls, ..Default::default() };
    out.sessions.push(SessionMeta { agent: Some(Agent::Codex), id: session, title: None, cwd, parent: None });
    out.quotas = quotas;
    Ok(out)
}

fn quota(w: &Value, observed: i64, limit: &str, plan: Option<String>) -> Option<QuotaWindow> {
    let used = w.get("used_percent")?.as_f64()?;
    let minutes = w.get("window_minutes").and_then(Value::as_u64).unwrap_or(0) as u32;
    let resets_at = match w.get("resets_at").and_then(Value::as_i64) {
        Some(t) => t,
        None => observed / 1000 + w.get("resets_in_seconds").and_then(Value::as_i64)?,
    };
    let mut label = match minutes {
        300 => "5-hour".to_string(),
        10080 => "Weekly".to_string(),
        m if m % 1440 == 0 && m > 0 => format!("{}-day", m / 1440),
        m if m % 60 == 0 && m > 0 => format!("{}-hour", m / 60),
        m => format!("{m}-minute"),
    };
    if limit != "codex" {
        label = format!("{label} · {limit}");
    }
    Some(QuotaWindow { agent: Agent::Codex, label, window_minutes: minutes, used_percent: used, resets_at, observed_at: observed, plan })
}

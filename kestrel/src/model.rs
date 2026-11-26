//! Core data types shared by parsers, analytics and the UI.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
    Antigravity,
    Pi,
    Grok,
    Magpie,
}

impl Agent {
    pub const ALL: [Agent; 6] = [Agent::Claude, Agent::Antigravity, Agent::Codex, Agent::Grok, Agent::Pi, Agent::Magpie];

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Antigravity => "Antigravity",
            Agent::Pi => "Pi",
            Agent::Grok => "Grok",
            Agent::Magpie => "Magpie",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Antigravity => "antigravity",
            Agent::Pi => "pi",
            Agent::Grok => "grok",
            Agent::Magpie => "magpie",
        }
    }

    pub fn from_slug(s: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|a| a.slug() == s)
    }
}

/// Token counts for one model call. `input` never includes cached tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub input: u64,
    /// Output tokens, reasoning included.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_read: u64,
    /// Cache writes billed at the short-TTL (5 minute) rate, or of unknown TTL.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write: u64,
    /// Cache writes billed at the 1 hour rate (Anthropic charges 2x input for these).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write_1h: u64,
    /// Reasoning tokens, a subset of `output`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reasoning: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write + self.cache_write_1h
    }

    /// Everything the model read for this call: the context size.
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.cache_write_1h
    }

    pub fn is_empty(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_write == 0 && self.cache_write_1h == 0
    }

    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cache_write_1h += o.cache_write_1h;
        self.reasoning += o.reasoning;
    }
}

/// One deduplicated model request.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Call {
    /// Global deduplication key, unique across files and agents.
    pub key: String,
    pub agent: Agent,
    /// Request start, unix milliseconds.
    pub ts: i64,
    pub session: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub tokens: Tokens,
    /// Time to first token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u32>,
    /// Wall time from request start to the last byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    /// Tokens decoded and the milliseconds they took, for output speed.
    /// Parsers fill this only when they can isolate the decode phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode: Option<(u32, u32)>,
    /// Cost the agent itself recorded, used when the model has no catalog price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_usd: Option<f64>,
    /// Upstream response id, for cross-source deduplication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Number of model calls folded into this record (Grok reports per turn).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub calls: u32,

    // Filled in by pricing, never cached.
    #[serde(skip)]
    pub usd: f64,
    /// What cache reads saved versus paying the input rate.
    #[serde(skip)]
    pub saved_usd: f64,
    #[serde(skip)]
    pub priced: crate::pricing::Match,
}

fn one() -> u32 {
    1
}
fn is_one(v: &u32) -> bool {
    *v == 1
}

impl Call {
    /// Output tokens per second over the decode phase, if measurable.
    pub fn tps(&self) -> Option<f64> {
        let (tokens, ms) = self.decode?;
        if tokens < 16 || ms < 250 {
            return None;
        }
        let tps = tokens as f64 / (ms as f64 / 1000.0);
        // Outside these bounds the window must include tool, queue or
        // network time rather than decoding alone.
        (2.0..2000.0).contains(&tps).then_some(tps)
    }
}

/// Builds the decode window from a total duration and time to first token.
pub fn decode_window(output: u64, duration_ms: Option<u32>, ttft_ms: Option<u32>) -> Option<(u32, u32)> {
    let (d, t) = (duration_ms?, ttft_ms?);
    (d > t).then(|| (output.min(u32::MAX as u64) as u32, d - t))
}

/// Per-session metadata gathered from logs and agent indexes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionMeta {
    pub agent: Option<Agent>,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// A rate-limit window as last reported by an agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct QuotaWindow {
    pub agent: Agent,
    /// Human label, e.g. "5 hours" or "Week".
    pub label: String,
    pub window_minutes: u32,
    pub used_percent: f64,
    /// Unix seconds.
    pub resets_at: i64,
    /// When this snapshot was observed, unix ms.
    pub observed_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

/// Everything extracted from one source file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileParse {
    pub calls: Vec<Call>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SessionMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotas: Vec<QuotaWindow>,
    /// Placeholder model names this file could resolve, for files that could not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_names: Vec<(String, String)>,
}

pub fn project_name(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(last) if !last.is_empty() => last.to_string(),
        _ => trimmed.to_string(),
    }
}

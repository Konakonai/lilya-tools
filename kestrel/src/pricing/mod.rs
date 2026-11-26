//! Live model prices from models.dev (OpenRouter as fallback), cached on disk,
//! plus user aliases and overrides from `~/.config/kestrel/prices.toml`.

pub mod normalize;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Call, Tokens};

const MODELS_DEV: &str = "https://models.dev/api.json";
const OPENROUTER: &str = "https://openrouter.ai/api/v1/models";
pub const MAX_AGE_SECS: i64 = 24 * 3600;

/// USD per million tokens.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Price {
    pub id: String,
    pub provider: String,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub reasoning: Option<f64>,
    /// Higher rates once the prompt exceeds `context` tokens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<Tier>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Tier {
    pub context: u64,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

impl Price {
    pub fn cost(&self, t: &Tokens) -> f64 {
        let prompt = t.prompt();
        let tier = self.tiers.iter().filter(|x| prompt > x.context).max_by_key(|x| x.context);
        let (input, output, cache_read, cache_write) = match tier {
            Some(x) => (x.input, x.output, x.cache_read.or(self.cache_read), x.cache_write.or(self.cache_write)),
            None => (self.input, self.output, self.cache_read, self.cache_write),
        };
        // Missing cache rates fall back to the common ratios: reads 10%, writes 125%.
        let cache_read = cache_read.unwrap_or(input * 0.1);
        let cache_write = cache_write.unwrap_or(input * 1.25);
        // One-hour cache writes cost twice the input rate (Anthropic).
        let cache_write_1h = input * 2.0;
        let (visible, reasoning) = match self.reasoning {
            Some(_) => (t.output.saturating_sub(t.reasoning), t.reasoning),
            None => (t.output, 0),
        };
        (t.input as f64 * input
            + visible as f64 * output
            + reasoning as f64 * self.reasoning.unwrap_or(output)
            + t.cache_read as f64 * cache_read
            + t.cache_write as f64 * cache_write
            + t.cache_write_1h as f64 * cache_write_1h)
            / 1_000_000.0
    }

    /// What the cache reads would have cost as plain input.
    pub fn cache_savings(&self, t: &Tokens) -> f64 {
        let rate = self.cache_read.unwrap_or(self.input * 0.1);
        t.cache_read as f64 * (self.input - rate).max(0.0) / 1_000_000.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Match {
    /// Same id after normalization.
    Exact,
    /// Matched after dropping effort / channel / deployment suffixes.
    Variant,
    /// User alias or price in prices.toml.
    Override,
    /// Cost recorded by the agent itself.
    Reported,
    #[default]
    None,
}

#[derive(Clone, Debug, Default)]
pub struct Resolution {
    pub price: Option<Price>,
    pub kind: Match,
}

#[derive(Serialize, Deserialize, Default)]
struct DiskCache {
    fetched_at: i64,
    models: HashMap<String, Price>,
}

#[derive(Deserialize, Default)]
struct OverrideFile {
    #[serde(default)]
    alias: HashMap<String, String>,
    #[serde(default)]
    model: HashMap<String, OverridePrice>,
}

#[derive(Deserialize, Default, Clone)]
struct OverridePrice {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

#[derive(Default)]
struct Inner {
    models: HashMap<String, Price>,
    /// Catalog keys, longest first, for prefix fallback.
    keys: Vec<String>,
    fetched_at: i64,
    aliases: HashMap<String, String>,
    overrides: HashMap<String, Price>,
    resolved: HashMap<String, Resolution>,
    last_error: Option<String>,
}

#[derive(Clone)]
pub struct Catalog {
    inner: Arc<RwLock<Inner>>,
    cache_path: PathBuf,
    override_path: PathBuf,
}

impl Catalog {
    pub fn open(cache_dir: &Path, config_dir: &Path) -> Catalog {
        let cat =
            Catalog { inner: Arc::default(), cache_path: cache_dir.join("prices.json"), override_path: config_dir.join("prices.toml") };
        if let Ok(b) = fs::read(&cat.cache_path)
            && let Ok(dc) = serde_json::from_slice::<DiskCache>(&b)
        {
            cat.install(dc.models, dc.fetched_at);
        }
        cat.reload_overrides();
        cat
    }

    pub fn override_path(&self) -> &Path {
        &self.override_path
    }

    pub fn reload_overrides(&self) {
        let file: OverrideFile = fs::read_to_string(&self.override_path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
        let mut g = self.inner.write().unwrap();
        g.aliases = file.alias.into_iter().map(|(k, v)| (normalize::canonical(&k), v)).collect();
        g.overrides = file
            .model
            .into_iter()
            .map(|(k, v)| {
                let id = normalize::canonical(&k);
                let p = Price {
                    id: k,
                    provider: "override".into(),
                    input: v.input,
                    output: v.output,
                    cache_read: v.cache_read,
                    cache_write: v.cache_write,
                    reasoning: None,
                    tiers: Vec::new(),
                };
                (id, p)
            })
            .collect();
        g.resolved.clear();
    }

    /// Adds an alias (`model = "target"`) to prices.toml, keeping the rest of the file.
    pub fn add_alias(&self, model: &str, target: &str) -> anyhow::Result<()> {
        let text = fs::read_to_string(&self.override_path).unwrap_or_default();
        let mut doc: toml::Table = toml::from_str(&text).unwrap_or_default();
        let alias = doc.entry("alias").or_insert_with(|| toml::Value::Table(Default::default()));
        let toml::Value::Table(t) = alias else { bail!("[alias] in prices.toml is not a table") };
        if target.is_empty() {
            t.remove(model);
        } else {
            t.insert(model.to_string(), toml::Value::String(target.to_string()));
        }
        if let Some(dir) = self.override_path.parent() {
            fs::create_dir_all(dir)?;
        }
        let header =
            "# Kestrel price overrides.\n# [alias] maps a model id to a catalog id; [model.\"id\"] sets USD per million tokens.\n\n";
        let body = toml::to_string_pretty(&doc)?;
        fs::write(&self.override_path, if text.trim().is_empty() { format!("{header}{body}") } else { body })?;
        self.reload_overrides();
        Ok(())
    }

    fn install(&self, models: HashMap<String, Price>, fetched_at: i64) {
        let mut keys: Vec<String> = models.keys().cloned().collect();
        keys.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        let mut g = self.inner.write().unwrap();
        g.models = models;
        g.keys = keys;
        g.fetched_at = fetched_at;
        g.resolved.clear();
    }

    pub fn status(&self) -> (usize, i64, Option<String>) {
        let g = self.inner.read().unwrap();
        (g.models.len(), g.fetched_at, g.last_error.clone())
    }

    pub fn is_stale(&self, now_secs: i64) -> bool {
        let g = self.inner.read().unwrap();
        g.models.is_empty() || now_secs - g.fetched_at > MAX_AGE_SECS
    }

    /// Downloads fresh prices. Blocking; call off the UI thread.
    pub fn refresh(&self) -> anyhow::Result<usize> {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().new_agent();
        let mut merged: HashMap<String, (i32, Price)> = HashMap::new();
        let mut errors = Vec::new();
        match fetch_json(&agent, MODELS_DEV).and_then(|v| parse_models_dev(&v)) {
            Ok(list) => {
                for (rank, p) in list {
                    let key = normalize::canonical(&p.id);
                    match merged.get(&key) {
                        Some((r, _)) if *r >= rank => {}
                        _ => {
                            merged.insert(key, (rank, p));
                        }
                    }
                }
            }
            Err(e) => errors.push(format!("models.dev: {e:#}")),
        }
        match fetch_json(&agent, OPENROUTER).and_then(|v| parse_openrouter(&v)) {
            Ok(list) => {
                for p in list {
                    merged.entry(normalize::canonical(&p.id)).or_insert((0, p));
                }
            }
            Err(e) => errors.push(format!("openrouter: {e:#}")),
        }
        if merged.is_empty() {
            let msg = errors.join("; ");
            self.inner.write().unwrap().last_error = Some(msg.clone());
            bail!(msg);
        }
        let models: HashMap<String, Price> = merged.into_iter().map(|(k, (_, p))| (k, p)).collect();
        let now = chrono::Utc::now().timestamp();
        let n = models.len();
        if let Some(dir) = self.cache_path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let dc = DiskCache { fetched_at: now, models };
        let tmp = self.cache_path.with_extension("json.tmp");
        if fs::write(&tmp, serde_json::to_vec(&dc)?).is_ok() {
            let _ = fs::rename(&tmp, &self.cache_path);
        }
        self.install(dc.models, now);
        self.inner.write().unwrap().last_error = (!errors.is_empty()).then(|| errors.join("; "));
        Ok(n)
    }

    pub fn resolve(&self, raw: &str) -> Resolution {
        if let Some(r) = self.inner.read().unwrap().resolved.get(raw) {
            return r.clone();
        }
        let r = {
            let g = self.inner.read().unwrap();
            resolve_in(&g, raw, 0)
        };
        self.inner.write().unwrap().resolved.insert(raw.to_string(), r.clone());
        r
    }

    /// Prices a call: user overrides and catalog first, then the agent's own figure.
    pub fn price(&self, c: &Call) -> (f64, Match, f64) {
        let r = self.resolve(&c.model);
        match (&r.price, c.reported_usd) {
            (_, Some(usd)) if r.kind != Match::Override && usd > 0.0 => {
                let savings = r.price.as_ref().map(|p| p.cache_savings(&c.tokens)).unwrap_or(0.0);
                (usd, Match::Reported, savings)
            }
            (Some(p), _) => (p.cost(&c.tokens), r.kind, p.cache_savings(&c.tokens)),
            (None, _) => (0.0, Match::None, 0.0),
        }
    }

    /// Catalog entries whose id contains `query`, for alias pickers.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Price> {
        let q = normalize::canonical(query);
        let g = self.inner.read().unwrap();
        let mut hits: Vec<&Price> = g.models.iter().filter(|(k, _)| q.is_empty() || k.contains(&q)).map(|(_, p)| p).collect();
        hits.sort_by(|a, b| a.id.len().cmp(&b.id.len()).then(a.id.cmp(&b.id)));
        hits.into_iter().take(limit).cloned().collect()
    }
}

fn resolve_in(g: &Inner, raw: &str, depth: u8) -> Resolution {
    let cands = normalize::candidates(raw);
    for (i, key) in cands.iter().enumerate() {
        if let Some(p) = g.overrides.get(key) {
            return Resolution { price: Some(p.clone()), kind: Match::Override };
        }
        if let Some(target) = g.aliases.get(key)
            && depth < 3
        {
            let mut r = resolve_in(g, target, depth + 1);
            if r.price.is_some() {
                r.kind = Match::Override;
            }
            return r;
        }
        if let Some(p) = g.models.get(key) {
            let kind = if i == 0 { Match::Exact } else { Match::Variant };
            return Resolution { price: Some(p.clone()), kind };
        }
    }
    // Longest catalog id that prefixes the name: grok-4-7-build-beta -> grok-4-7.
    if let Some(base) = cands.last() {
        for k in &g.keys {
            if k.len() >= 6 && base.starts_with(k.as_str()) && base.as_bytes().get(k.len()) == Some(&b'-') {
                return Resolution { price: g.models.get(k).cloned(), kind: Match::Variant };
            }
        }
    }
    Resolution::default()
}

fn fetch_json(agent: &ureq::Agent, url: &str) -> anyhow::Result<Value> {
    let mut resp = agent.get(url).header("User-Agent", "kestrel/0.1").call().with_context(|| format!("GET {url}"))?;
    let bytes = resp.body_mut().with_config().limit(64 << 20).read_to_vec()?;
    let v: Value = serde_json::from_slice(&bytes)?;
    Ok(v)
}

/// First-party listings win over resellers for the same model id.
fn provider_rank(provider: &str, id: &str) -> i32 {
    let p = provider.to_ascii_lowercase();
    let id = id.to_ascii_lowercase();
    let first_party = [
        ("claude", &["anthropic"][..]),
        ("gpt", &["openai"]),
        ("o1", &["openai"]),
        ("o3", &["openai"]),
        ("o4", &["openai"]),
        ("codex", &["openai"]),
        ("gemini", &["google"]),
        ("gemma", &["google"]),
        ("grok", &["xai"]),
        ("deepseek", &["deepseek"]),
        ("kimi", &["moonshotai", "moonshot"]),
        ("glm", &["zhipuai", "zai"]),
        ("qwen", &["alibaba", "dashscope"]),
        ("minimax", &["minimax"]),
        ("mistral", &["mistral"]),
    ];
    for (prefix, owners) in first_party {
        if id.starts_with(prefix) && owners.contains(&p.as_str()) {
            return 100;
        }
    }
    match p.as_str() {
        "anthropic" | "openai" | "google" | "xai" | "deepseek" | "mistral" | "moonshotai" | "zhipuai" | "minimax" => 80,
        "google-vertex" | "amazon-bedrock" | "azure" => 50,
        "openrouter" => 30,
        _ => 20,
    }
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

fn parse_models_dev(v: &Value) -> anyhow::Result<Vec<(i32, Price)>> {
    let providers = v.as_object().context("models.dev: expected an object")?;
    let mut out = Vec::new();
    for (prov, pv) in providers {
        let Some(models) = pv.get("models").and_then(Value::as_object) else { continue };
        for (key, m) in models {
            let Some(cost) = m.get("cost") else { continue };
            let (input, output) = (num(cost, "input").unwrap_or(0.0), num(cost, "output").unwrap_or(0.0));
            if input <= 0.0 && output <= 0.0 {
                continue;
            }
            let id = m.get("id").and_then(Value::as_str).unwrap_or(key).to_string();
            let mut tiers = Vec::new();
            if let Some(list) = cost.get("tiers").and_then(Value::as_array) {
                for t in list {
                    let ctx = t.pointer("/tier/size").and_then(Value::as_u64);
                    if t.pointer("/tier/type").and_then(Value::as_str) == Some("context")
                        && let Some(ctx) = ctx
                    {
                        tiers.push(Tier {
                            context: ctx,
                            input: num(t, "input").unwrap_or(input),
                            output: num(t, "output").unwrap_or(output),
                            cache_read: num(t, "cache_read"),
                            cache_write: num(t, "cache_write"),
                        });
                    }
                }
            }
            if tiers.is_empty()
                && let Some(t) = cost.get("context_over_200k")
            {
                tiers.push(Tier {
                    context: 200_000,
                    input: num(t, "input").unwrap_or(input),
                    output: num(t, "output").unwrap_or(output),
                    cache_read: num(t, "cache_read"),
                    cache_write: num(t, "cache_write"),
                });
            }
            let rank = provider_rank(prov, &id);
            out.push((
                rank,
                Price {
                    id,
                    provider: prov.clone(),
                    input,
                    output,
                    cache_read: num(cost, "cache_read"),
                    cache_write: num(cost, "cache_write"),
                    reasoning: num(cost, "reasoning"),
                    tiers,
                },
            ));
        }
    }
    Ok(out)
}

fn parse_openrouter(v: &Value) -> anyhow::Result<Vec<Price>> {
    let data = v.get("data").and_then(Value::as_array).context("openrouter: no data")?;
    let per_m = |p: &Value, k: &str| p.get(k).and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok()).map(|x| x * 1e6);
    let mut out = Vec::new();
    for m in data {
        let (Some(id), Some(p)) = (m.get("id").and_then(Value::as_str), m.get("pricing")) else { continue };
        let (input, output) = (per_m(p, "prompt").unwrap_or(0.0), per_m(p, "completion").unwrap_or(0.0));
        if input <= 0.0 && output <= 0.0 {
            continue;
        }
        out.push(Price {
            id: id.to_string(),
            provider: id.split('/').next().unwrap_or("openrouter").to_string(),
            input,
            output,
            cache_read: per_m(p, "input_cache_read").filter(|x| *x > 0.0),
            cache_write: per_m(p, "input_cache_write").filter(|x| *x > 0.0),
            reasoning: None,
            tiers: Vec::new(),
        });
    }
    Ok(out)
}

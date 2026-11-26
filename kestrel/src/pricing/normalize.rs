//! Model id normalization for price lookups.
//!
//! Agents report names like `antigravity/gemini-4-neon-high`, `grok-4.7-build`,
//! `claude-opus-5-5[1m]` or `gemini-3.8-flash-n`, while catalogs list
//! `gemini-4-neon`, `grok-4.7` and `claude-opus-5-5`. `candidates` yields
//! progressively simpler ids to try, most specific first.

/// Lowercases, drops routing prefixes and tags, and unifies separators.
/// Dots between digits become dashes, so `gpt-6.2` and `gpt-6-2` meet.
pub fn canonical(raw: &str) -> String {
    let mut s = raw.trim().to_ascii_lowercase();
    if let Some(i) = s.rfind('/') {
        s = s[i + 1..].to_string();
    }
    if let Some(i) = s.find('[') {
        s.truncate(i);
    }
    if let Some(i) = s.find(':')
        && i > 0
    {
        s.truncate(i);
    }
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let c = match c {
            ' ' | '_' => '-',
            // 4.6 -> 4-6, and Fireworks' k3p1 -> k3-1.
            '.' | 'p' if i > 0 && i + 1 < chars.len() && chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit() => '-',
            c => c,
        };
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    strip_dates(out.trim_matches('-'))
}

fn strip_dates(s: &str) -> String {
    let mut s = s.to_string();
    loop {
        let before = s.len();
        // -20260801 / @20260801
        if let Some(i) = s.rfind(['-', '@']) {
            let tail = &s[i + 1..];
            let is_date8 = tail.len() == 8 && tail.starts_with("20") && tail.bytes().all(|b| b.is_ascii_digit());
            let is_date6 = tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_digit()) && (&tail[2..4] >= "01" && &tail[2..4] <= "12");
            if is_date8 || (is_date6 && i > 0) {
                s.truncate(i);
            }
        }
        // -2026-08-01
        if s.len() > 11 {
            let tail = &s[s.len() - 11..];
            let b = tail.as_bytes();
            if b[0] == b'-'
                && b[5] == b'-'
                && b[8] == b'-'
                && tail[1..3] == *"20"
                && tail[1..].bytes().filter(|c| c.is_ascii_digit()).count() == 8
            {
                s.truncate(s.len() - 11);
            }
        }
        if s.len() == before {
            return s;
        }
    }
}

/// Suffixes that select effort, channel or deployment but not price.
const VARIANTS: &[&str] = &[
    "-extended-thinking",
    "-non-thinking",
    "-nothinking",
    "-thinking",
    "-reasoning",
    "-computer-use",
    "-preview",
    "-latest",
    "-tiered",
    "-minimal",
    "-xhigh",
    "-medium",
    "-high",
    "-low",
    "-max",
    "-build",
    "-trial",
    "-agent",
    "-alpha",
    "-beta",
    "-fast",
    "-chat",
    "-exp",
    "-dev",
];

/// Candidate catalog keys for `raw`, most specific first.
pub fn candidates(raw: &str) -> Vec<String> {
    let base = canonical(raw);
    let mut out = Vec::new();
    let mut push = |v: String| {
        let v = v.trim_matches('-').to_string();
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    };
    push(base.clone());
    let mut cur = base;
    for _ in 0..6 {
        let next = peel(&cur);
        if next == cur {
            break;
        }
        push(next.clone());
        cur = next;
    }
    out
}

fn peel(s: &str) -> String {
    // Single-letter deployment tags: gemini-3-8-flash-n, gemini-3-flash-a.
    if let Some((head, tail)) = s.rsplit_once('-')
        && tail.len() == 1
        && tail.as_bytes()[0].is_ascii_lowercase()
        && head.contains('-')
    {
        return strip_dates(head);
    }
    for v in VARIANTS {
        if let Some(head) = s.strip_suffix(v)
            && head.len() > 2
        {
            return strip_dates(head);
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes() {
        assert_eq!(canonical("anthropic/Claude-Opus-5.5"), "claude-opus-5-5");
        assert_eq!(canonical("claude-opus-5-5[1m]"), "claude-opus-5-5");
        assert_eq!(canonical("gpt-6.2-astra"), "gpt-6-2-astra");
        assert_eq!(canonical("claude-sonnet-4-5-20250929"), "claude-sonnet-4-5");
        assert_eq!(canonical("gpt-4o-2024-08-06"), "gpt-4o");
        assert_eq!(canonical("deepseek/deepseek-v4:free"), "deepseek-v4");
        assert_eq!(canonical("accounts/fireworks/models/kimi-k3p1"), "kimi-k3-1");
    }

    #[test]
    fn peels_variants() {
        let c = candidates("gemini-4-hydrogen-tiered-high");
        assert!(c.contains(&"gemini-4-hydrogen".to_string()), "{c:?}");
        let c = candidates("gemini-3.8-flash-n");
        assert!(c.contains(&"gemini-3-8-flash".to_string()), "{c:?}");
        let c = candidates("grok-4.7-build");
        assert_eq!(c[1], "grok-4-7");
        let c = candidates("gemini-4-argon-trial");
        assert_eq!(c[1], "gemini-4-argon");
    }
}

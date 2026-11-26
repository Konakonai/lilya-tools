# lilya-tools

Standalone plugins and scripts extracted from [`nixos-config`](https://github.com/Konakonai/nixos-config) so they can evolve independently and be consumed as a single Nix flake input.

## Contents

- `kestrel/`: GTK4 + libadwaita monitor (Rust) for local AI coding agents — Claude Code (incl. Claude Desktop agent mode), Codex (incl. zstd-archived rollouts), Antigravity, Pi, Grok and historical Magpie logs. Live inotify updates; cross-file request deduplication; subagent conversations folded into their parent session; models.dev + OpenRouter pricing with 5-minute/1-hour cache-write rates and user aliases (`~/.config/kestrel/prices.toml`); TTFT and decode-only tok/s distributions; Codex rate-limit windows. `kestrel report` and `kestrel status` print the same numbers in the terminal (JSON for status bars).
- `music-discover/`: Weekly music discovery page generator seeding from NetEase Cloud Music and aggregating NetEase, ListenBrainz, and YouTube Music recommendations (`music-discover/tests/` holds unit tests, wired into `nix flake check`).
- `beets-zhlyrics/`: Beets plugin adding NetEase lyrics search, bilingual Chinese/Traditional matching on LRCLib, OpenCC script normalization, and local `.lrc` / `UNSYNCEDLYRICS` support.
- `claude-token-band/`: Claude Code plugin rendering tok/s, TTFT, prompt cache hit rate, and cache TTL countdown above the prompt (`claude plugin test claude-token-band`).

## Checks

```bash
nix flake check
```

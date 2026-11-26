# lilya-tools

Standalone plugins and scripts extracted from [`nixos-config`](https://github.com/Konakonai/nixos-config) so they can evolve independently and be consumed as a single Nix flake input.

## Contents

- `finch/`: Native MyGo (`mygo/ui`) desktop monitor for local AI coding agent token usage, estimated cost, output TPS, and TTFT/duration latency across Claude, Codex, Antigravity, Pi, Grok, and historical Magpie logs — with live `models.dev` + OpenRouter pricing sync, automatic suffix normalization, and Catppuccin Lavender Light styling.
- `music-discover/`: Weekly music discovery page generator seeding from NetEase Cloud Music and aggregating NetEase, ListenBrainz, and YouTube Music recommendations (`music-discover/tests/` holds unit tests, wired into `nix flake check`).
- `beets-zhlyrics/`: Beets plugin adding NetEase lyrics search, bilingual Chinese/Traditional matching on LRCLib, OpenCC script normalization, and local `.lrc` / `UNSYNCEDLYRICS` support.
- `claude-token-band/`: Claude Code plugin rendering tok/s, TTFT, prompt cache hit rate, and cache TTL countdown above the prompt (`claude plugin test claude-token-band`).

## Checks

```bash
nix flake check
```

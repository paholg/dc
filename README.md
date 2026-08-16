# devconcurrent

[![CI](https://github.com/paholg/devconcurrent/actions/workflows/ci.yml/badge.svg)](https://github.com/paholg/devconcurrent/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/paholg/devconcurrent)](https://github.com/paholg/devconcurrent/releases/latest)
[![Book](https://img.shields.io/badge/book-devconcurrent.paholg.com-blue?logo=mdbook)](https://devconcurrent.paholg.com)

<!-- OUTLINE — replace with prose. Marketing-shaped: why + excitement; all
instruction lives in the book. Target length: fits on one screen-and-a-half.

- Tagline under the title. Old one ("Development environments made easy") is
  generic; candidates lean on the actual promise: "one command per branch:
  worktree, container, and its own https URL" or similar.
- Badges are in place above (CI, latest release, book).

## The hook (no heading, first paragraph)
- The pain, concretely: you want N branches running at once — reviews, spikes,
  parallel AI agents — and what you get is directory juggling, container
  collisions, and port roulette.
- The promise in one line: `dc up foo` → isolated worktree + devcontainer;
  `dc destroy foo` → gone.

## Demo (the centerpiece)
- A short terminal session, ~8 lines: up foo -g → x → (run app) → open
  https://foo.app.test → status showing foo & bar side by side → destroy.
- Placeholder for a gif/asciinema/VHS recording — worth the effort for the
  "excitement" goal; a static code block is the fallback.

## What you get (feature bullets, one line each)
- Worktrees as cheap, disposable workspaces (cattle, not pets).
- A real CLI for devcontainers: up, exec, compose, destroy.
- Per-workspace hostnames — never think about ports (foo.app.test, bar.app.test).
- TLS termination — real https in your browser, via mkcert.
- Layers: use any subset; each is optional. (The ogre line can live here.)
- Env vars that follow your cwd (DATABASE_URL per workspace).

## Why-now angle (consider)
- Parallel AI coding agents are the moment for this tool: each agent in its
  own workspace/container, each reviewable at its own URL. Decide how loud to
  make this; even one sentence widens the audience.

## Get started
- Install one-liner (mise) + releases link + nix mention.
- 3-line config.toml.
- `dc up foo`.
- Then: link to the book (Quick Start) — the only doc link that matters here.
  Full DNS/TLS setup explicitly deferred to the book with a "15 minutes,
  worth it" framing.

## Status
- Experimental / pre-0.1.0 note (port from old README L3–5, keep it short).

## Cut from old README (now in the book — do not re-add here):
- Config location table, shell setup snippets, all DNS/platform setup, proxy
  config, tips, CLI quirks, glossary.

## GAP: repo has no LICENSE file; READMEs usually end with a license line.
-->

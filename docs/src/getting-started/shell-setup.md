# Shell Setup

<!-- OUTLINE — replace with prose.
Source: README-old.md "Shell Setup" (L45–65), plus prompt-hook bit from
"Workspace variables" (L466–475).

- Why: completions + the `dc` function. `dc` vs `devconcurrent`: only `dc` can
  cd you (and set env vars in your shell).
- Per-shell snippets: bash, elvish, fish, zsh. Note elvish is completions-only.
- What the sourced code actually does (GAP — undocumented today): defines the
  `dc` function, installs completions, and (if `shell.exportEnv = true`)
  registers a prompt hook.
- Convention note: the rest of the book writes `dc`.
-->

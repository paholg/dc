# CLI Reference

<!-- OUTLINE — replace this comment with prose; the command reference below is
generated and stays.

- Global behavior (hand-written):
  - `dc` vs `devconcurrent` (function powers: cd, env export).
  - Project resolution order: --project flag → DC_PROJECT → cwd → first
    configured. Alias pattern (`alias dcb='dc -p bippity'`).
  - Workspace defaulting: commands taking a workspace default to the one
    containing cwd.
  - Completions: the COMPLETE env var mechanism.
- The generated section below comes from the clap definitions
  (`just gen`); improve it by editing doc comments in crates/cli/src/cli/.
-->

{{#include generated/cli.md}}

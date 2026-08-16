# Workspaces

<!-- OUTLINE — replace with prose.
Source: README-old.md "Workspaces" (L156–172).

- What a workspace is (worktree + optional devcontainer); root workspace.
- `dc up NAME` / `-g`; `dc destroy NAME`; `dc status`; `dc go NAME`.
- Cattle-not-pets: one workspace = one branch = one PR; link to Cache Volumes
  for making creation cheap.
- Alias suggestions (`d` for `dc go`, completions make paths irrelevant).
- GAPS (undocumented today, worth writing here):
  - Branch semantics of `dc up`: what branch is created, from what base; what
    happens if the branch already exists.
  - `dc destroy` safety check: refuses when git is dirty; `--force` overrides.
    (Exists in code, absent from old README.)
  - What operations mean on the root workspace (can you destroy it? does
    `dc up` with no args do anything?).
  - Where worktrees live by default (`worktreeFolder`, platform data dir).
-->

# Devcontainers

<!-- OUTLINE — replace with prose.
Source: README-old.md "Devcontainers" (L174–224).

- Scope statement up front: compose-based only, no features; link to the
  Devcontainer Compatibility chapter for details and rationale.
- How the workspace commands change with a devcontainer:
  - `dc up`: brings up the container, recreates existing, runs lifecycle
    commands. GAP: define "recreate" — what's preserved, what's rebuilt, when
    images rebuild.
  - `dc destroy`: removes containers and volumes.
  - `dc status`: docker info, `--live`, `--workspace`.
  - `dc show`: one-line mention, link to CLI reference.
- New commands:
  - `dc exec` / `dc x` (and `defaultExec`, possibly going away).
  - `dc fwd` / `dc f`: one line, link to Port Forwarding chapter.
  - `dc compose` / `dc c`: passthrough, e.g. `dc c logs -f`.
- Customizations overview (`customizations.devconcurrent`), where it can live
  (devcontainer.json vs config.toml per-user override) — brief, link to
  reference.
- GAP: which lifecycle commands run, in what order, and where (host vs
  container).
-->

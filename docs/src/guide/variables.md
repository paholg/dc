# Workspace Variables

<!-- OUTLINE — replace with prose.
Source: README-old.md "Workspace variables" (L438–494).

- Motivation: your app needs URLs that differ per workspace
  (APP_URL, DATABASE_URL).
- `customizations.devconcurrent.env` example with `{{hostname 'svc'}}`.
- `dc show env` table output.
- Getting them into your shell:
  - Automatic: `shell.exportEnv = true` + prompt hook.
  - Manual: `dc show env --export=bash`; `eval` form without the function.
  - These are host-side variables — clarify vs containerEnv/remoteEnv (GAP:
    old README has one line; readers will ask how to get these INTO the
    container).
- Hook debugging note (silent outside a workspace; template errors always
  reported).
-->

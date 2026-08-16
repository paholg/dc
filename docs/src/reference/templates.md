# Templates Reference

<!-- OUTLINE — replace with prose. NEW CHAPTER: hostname templates and env
templates are described twice in the old docs (README L402–436, L438–457;
CONFIGURATION.md L94–111); consolidate the handlebars details once, here.

- Both hostname and env values are handlebars templates.
- Variables: `root`, `project`, `workspace`; hostname templates add `service`.
- Helper: `{{hostname 'svc'}}` (env templates only).
- Defaults: hostname = `{{workspace}}.{{service}}.test`; changing the TLD.
- Per-service hostname override example (drop the service segment).
- Rendering strictness: env templates error on unknown variables; hostname
  templates don't (state this contrast explicitly — GAP, currently only the
  env side is stated).
-->

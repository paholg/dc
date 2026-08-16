# Devcontainer Customizations Reference

<!-- OUTLINE — replace with prose.
Source: CONFIGURATION.md L75–113 (customizations.devconcurrent).

- Where these live: devcontainer.json or projects.NAME.devcontainer
  (repo-wide vs per-user).
- Options:
  - `defaultExec` (note: may be removed)
  - `worktreeFolder` (why it exists redundantly with config.toml)
  - `mountGit` [true] (what breaks without it)
  - `proxy.enable`, `proxy.hostname`, `proxy.services.SVC.hostname`,
    `proxy.services.SVC.ports[]` (ip, host, container, tls)
  - `env.VAR` (strict rendering; link to Templates)
- Fix while porting: CONFIGURATION.md L87 typo "projects and set it" →
  "projects can set it".
-->

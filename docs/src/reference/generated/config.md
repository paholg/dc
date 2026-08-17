* `projects.<name>` (table) — Configured projects by name.
  * `path` (string, required) — The project location on your host.
  * `worktreeFolder` (string) — The directory where devconcurrent will place worktrees. Defaults to the platform data directory. This is also settable in the devcontainer, but it's available here for projects that don't use devcontainers.
  * `devcontainer` (table) — Any of the options from `devcontainer.json` (<https://containers.dev/implementors/json_reference/>), as per-user overrides. These are merged with the project's `devcontainer.json`, with arrays concatenated and this file winning conflicts.
* `proxy` (table) — Global proxy settings.
  * `port` (integer) [default: `43770`] — The DNS port the proxy listens on.
  * `caRoot` (string) — Path to your CA root directory on the host. Find it with `mkcert -CAROOT`.
* `shell` (table) — Shell-integration settings.
  * `exportEnv` (boolean) [default: `true`] — Register a prompt hook to auto-set the variables from `customizations.devconcurrent.env` based on your current working directory.

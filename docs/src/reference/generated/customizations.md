* `worktreeFolder` (string) — The directory where devconcurrent will place worktrees.
* `mountGit` (boolean) [default: `true`] — Whether to mount the project's git directory into each workspace's devcontainer.

  Git worktrees have a simple `.git` file that points to the actual `.git` directory. If that
  directory isn't available, then no git commands will work. By mounting it at its original
  path in the devcontainer, `git` should just work, both inside and out of the container.
* `proxy` (table) — Configure DNS hostnames and HTTP proxy.
  * `enable` (boolean) [default: `false`] — Enable the devconcurrent DNS and HTTP proxy for this project.
  * `hostname` (string) [default: `"{{workspace}}.{{service}}.test"`] — Handlebars template for the proxied hostname, used by every service that does not set its own.

    Available variables:
    - `root` (bool) — whether this is the root workspace
    - `project` — project name
    - `workspace` — workspace name
    - `service` — name of the service from compose
  * `services.<name>` (table) — Configure proxy settings for each docker compose service.
    * `hostname` (string) — Handlebars template for this service's hostname. Overrides the project-level `hostname`.
    * `containerPort` (integer) — If set, devconcurrent will run an HTTP proxy on ports 80 and 443 to this port in your container, performing TLS termination on 443.

      If this service runs a web service, put its port here.

      All ports other than 80 and 443 are forwarded raw to the service, whether
      this is set or not.
* `env.<name>` (string) — Define shell variables

  These are rendered by `dc show env` or automatically set if `shell.exportEnv` is true.

  The values are given by handlebars templates with the following:
    * The `hostname` helper gives the hostname for a service.
    * The following variables are populated: `project`, `workspace`, and `root`.

  Example:

  ```json
  {
    "BASE_URL": "{{ hostname 'app' }}",
    "DATABASE_URL": "postgres://postgres:postgres@{{hostname 'postgres'}}:5432/db"
  }
  ```

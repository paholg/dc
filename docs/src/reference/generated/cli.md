# CLI

This document contains the help content for the `devconcurrent` command-line program.

**Command Overview:**

* [`devconcurrent`↴](#devconcurrent)
* [`devconcurrent up`↴](#devconcurrent-up)
* [`devconcurrent exec`↴](#devconcurrent-exec)
* [`devconcurrent fwd`↴](#devconcurrent-fwd)
* [`devconcurrent fwd stop`↴](#devconcurrent-fwd-stop)
* [`devconcurrent compose`↴](#devconcurrent-compose)
* [`devconcurrent destroy`↴](#devconcurrent-destroy)
* [`devconcurrent show`↴](#devconcurrent-show)
* [`devconcurrent show ports`↴](#devconcurrent-show-ports)
* [`devconcurrent show workspace`↴](#devconcurrent-show-workspace)
* [`devconcurrent show ip`↴](#devconcurrent-show-ip)
* [`devconcurrent show hostname`↴](#devconcurrent-show-hostname)
* [`devconcurrent show env`↴](#devconcurrent-show-env)
* [`devconcurrent show ca-root`↴](#devconcurrent-show-ca-root)
* [`devconcurrent status`↴](#devconcurrent-status)
* [`devconcurrent go`↴](#devconcurrent-go)
* [`devconcurrent proxy`↴](#devconcurrent-proxy)
* [`devconcurrent proxy up`↴](#devconcurrent-proxy-up)
* [`devconcurrent proxy down`↴](#devconcurrent-proxy-down)
* [`devconcurrent proxy status`↴](#devconcurrent-proxy-status)
* [`devconcurrent proxy trust`↴](#devconcurrent-proxy-trust)
* [`devconcurrent proxy untrust`↴](#devconcurrent-proxy-untrust)

## `devconcurrent`

A tool for managing devcontainers, especially when combined with git worktrees

**Usage:** `devconcurrent [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `up` — Bring up a workspace, creating it if it does not exist
* `exec` — Exec into a running devcontainer
* `fwd` — Forward configured `forwardPorts` to a running workspace
* `compose` — Run `docker compose` against the given workspace
* `destroy` — Fully destroy the workspace; equivalent to `docker compose down -v --rmi local --remove-orphans && git worktree remove`
* `show` — Show some value
* `status` — Show status for all workspaces in the project; container stats are aggregated
* `go` — Cd into the workspace directory (only if using via shell wrapper)
* `proxy` — Manage the DNS server and HTTP proxy

###### **Options:**

* `-p`, `--project <PROJECT>` — name of project [default: The DEVCONCURRENT_PROJECT variable, then the first configured project]



## `devconcurrent up`

Bring up a workspace, creating it if it does not exist

**Usage:** `devconcurrent up [OPTIONS] [WORKSPACE]`

###### **Arguments:**

* `<WORKSPACE>` — Workspace name

###### **Options:**

* `-f`, `--forward` — Foward configured `forwardPorts` once up
* `-d`, `--detach` — Detach worktree rather than creating a branch
* `-b`, `--branch <BRANCH>` — Specify a branch instead of using the worktree name
* `-g`, `--go` — Navigate to the directory after creating (if using via shell wrapper)
* `-x`, `--exec <EXEC>` — Exec once up with the given command [default: the container user's shell]



## `devconcurrent exec`

Exec into a running devcontainer

**Usage:** `devconcurrent exec [OPTIONS] [CMD]...`

**Command Alias:** `x`

###### **Arguments:**

* `<CMD>` — command to run [default: the container user's shell]

###### **Options:**

* `-w`, `--workspace <WORKSPACE>` — Workspace name [default: current working directory]



## `devconcurrent fwd`

Forward configured `forwardPorts` to a running workspace

**Usage:** `devconcurrent fwd [OPTIONS] [COMMAND]`

**Command Alias:** `f`

###### **Subcommands:**

* `stop` — Stop forwarding ports (remove sidecar containers)

###### **Options:**

* `-w`, `--workspace <WORKSPACE>` — Workspace name [default: current working directory]



## `devconcurrent fwd stop`

Stop forwarding ports (remove sidecar containers)

**Usage:** `devconcurrent fwd stop`



## `devconcurrent compose`

Run `docker compose` against the given workspace

**Usage:** `devconcurrent compose [OPTIONS] [ARGS]...`

**Command Alias:** `c`

###### **Arguments:**

* `<ARGS>` — Arguments to provide to `docker compose`

###### **Options:**

* `-w`, `--workspace <WORKSPACE>` — Workspace name [default: current working directory]



## `devconcurrent destroy`

Fully destroy the workspace; equivalent to `docker compose down -v --rmi local --remove-orphans && git worktree remove`

**Usage:** `devconcurrent destroy [OPTIONS] [WORKSPACE]`

###### **Arguments:**

* `<WORKSPACE>` — Workspace name

###### **Options:**

* `-f`, `--force` — Force remove the worktree, even if dirty



## `devconcurrent show`

Show some value

**Usage:** `devconcurrent show <COMMAND>`

###### **Subcommands:**

* `ports` — Show currently-forwarded ports for this workspace
* `workspace` — Print the current workspace name, or exit 1
* `ip` — Show container IP addresses for this workspace
* `hostname` — Show proxied hostnames for this workspace
* `env` — Show this workspace's configured shell variables
* `ca-root` — Print the CA root directory, generating the CA if it isn't there yet



## `devconcurrent show ports`

Show currently-forwarded ports for this workspace

**Usage:** `devconcurrent show ports`



## `devconcurrent show workspace`

Print the current workspace name, or exit 1

**Usage:** `devconcurrent show workspace`



## `devconcurrent show ip`

Show container IP addresses for this workspace

**Usage:** `devconcurrent show ip [SERVICE]`

###### **Arguments:**

* `<SERVICE>` — Compose service name; if omitted, list all services for this workspace



## `devconcurrent show hostname`

Show proxied hostnames for this workspace

**Usage:** `devconcurrent show hostname [SERVICE]`

###### **Arguments:**

* `<SERVICE>` — Compose service name; if omitted, list every service in this workspace's compose configuration



## `devconcurrent show env`

Show this workspace's configured shell variables

**Usage:** `devconcurrent show env [OPTIONS]`

###### **Options:**

* `--export <SHELL>` — Set the variables in the calling shell, in this shell's syntax, instead of printing a table.

   With the `dc` shell function sourced, this takes effect directly. Otherwise the assignments go to stdout for you to `eval`. Setting `shell.exportEnv` in config.toml wires this up on every prompt for you.

  Possible values: `bash`, `elvish`, `fish`, `powershell`, `zsh`




## `devconcurrent show ca-root`

Print the CA root directory, generating the CA if it isn't there yet

Trust the CA with `dc proxy trust`; this command is for importing `rootCA.pem` somewhere manually.

**Usage:** `devconcurrent show ca-root`



## `devconcurrent status`

Show status for all workspaces in the project; container stats are aggregated

**Usage:** `devconcurrent status [OPTIONS]`

**Command Alias:** `s`

###### **Options:**

* `-w`, `--workspace <WORKSPACE>` — Split by container, showing a single workspace
* `-l`, `--live` — Show live, updating data



## `devconcurrent go`

Cd into the workspace directory (only if using via shell wrapper)

**Usage:** `devconcurrent go <WORKSPACE>`

###### **Arguments:**

* `<WORKSPACE>` — Workspace name



## `devconcurrent proxy`

Manage the DNS server and HTTP proxy

**Usage:** `devconcurrent proxy <COMMAND>`

###### **Subcommands:**

* `up` — Start or restart the proxy
* `down` — Stop and remove the proxy
* `status` — Check that every configured hostname and port is reachable
* `trust` — Install the CA into the system and browser trust stores
* `untrust` — Remove the CA from the system and browser trust stores



## `devconcurrent proxy up`

Start or restart the proxy

**Usage:** `devconcurrent proxy up [OPTIONS]`

###### **Options:**

* `-w`, `--workspace <WORKSPACE>` — Workspace name (only useful if its devcontainer.json diverges from the root workspace)



## `devconcurrent proxy down`

Stop and remove the proxy

**Usage:** `devconcurrent proxy down`



## `devconcurrent proxy status`

Check that every configured hostname and port is reachable

**Usage:** `devconcurrent proxy status [OPTIONS]`

**Command Alias:** `s`

###### **Options:**

* `-w`, `--workspace <WORKSPACE>` — Workspace name (only useful if its devcontainer.json diverges from the root workspace)
* `-a`, `--all` — Check every proxy-enabled project, not just this one
* `--json` — Print the results as JSON instead of a table



## `devconcurrent proxy trust`

Install the CA into the system and browser trust stores

We will do our best to install our CA to your system trust store and, if possible, Firefox and Chromium's trust stores. This requires root, so you will be asked for your password via `sudo`.

Under WSL, the CA is also installed into your Windows user certificate store (via `certutil.exe`), so native Windows browsers trust it too; Windows will ask you to confirm in a security dialog.

Note that our CA is only valid for the listed TLDs (default is only "test"). As long as these aren't real TLDs that can serve real traffic, this is pretty safe, but it's still not recommended on a production machine.

**Usage:** `devconcurrent proxy trust`



## `devconcurrent proxy untrust`

Remove the CA from the system and browser trust stores

This will also delete the CA and its key.

**Usage:** `devconcurrent proxy untrust`




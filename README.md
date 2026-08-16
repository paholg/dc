# devconcurrent - Development environments made easy

**NOTE:** This is new, experimental software. Use at your own risk. It's still
undergoing rapid breaking changes; expect this to slow down once above version
0.1.0, and to mostly stop at 1.0.0.

## Value Proposition

With git worktrees, you can work on multiple branches of a project in different
directories at the same time.

With devcontainers, you _can_ have isolated development environments, but there
isn't great tooling outside of VSCode, and the reality of ports make this harder
than it should be.

Enter `devconcurrent`, a tool to easily manage worktrees + devcontainers; it can
bring them up, take them down, exec into them, etc all with simple CLI commands.

With `dc up foo` you have a brand new worktree named `foo`, running its
devcontainer, ready for you! Once you're done, just `dc destroy foo`.

On top of that, devconcurrent can give you a DNS server and TLS-terminating proxy.
With a little bit of setup, if you have some web app `app` and are working on
`feature3`, you can view it in your browser at `https://feature3.app.test`!

## Prerequisites

The only real requirement of `devconcurrent` is that you have git-enabled
project. But if you want to use all of the features of `devconcurrent`, you'll
need a modern `docker`, and will have to do a little bit of work to enable
devconcurrent's DNS server and to install a CA. See [Layers](#layers) for
details.

## Installation and Setup

### Installation

See <https://github.com/paholg/devconcurrent/releases>

In addition, tools that can install from standard GitHub releases, like mise,
should work. With `mise` specifically: `mise install github:paholg/devconcurrent`.

There is also a `nix` flake if you are a nix user.

### Shell Setup

Installation will install the `devconcurrent` binary. I recommend you then place
the following in your shell config file (e.g. `.bashrc`) to get nice completions
and the function `dc`. For the duration of this README, it is assumed you have
done that, but if you do not want, the only difference between calling `dc` and
`devconcurrent` is that `dc` has the power to cd you into workspaces.

```sh
# bash:
source <(COMPLETE=bash devconcurrent)

# elvish (just generates completions for now):
eval (E:COMPLETE=elvish devconcurrent | slurp)

# fish:
COMPLETE=fish devconcurrent | source

# zsh:
source <(COMPLETE=zsh devconcurrent)
```

### Configuration

In order to give you a nice experience, we require a very simple configuration
file that just lists your projects.

Devconcurrent reads `config.toml` from the config directory reported by the
[`directories`](https://docs.rs/directories) crate. That is not the same place
on every platform:

| Platform | Path                                                      |
| -------- | --------------------------------------------------------- |
| Linux    | `$XDG_CONFIG_HOME/devconcurrent/config.toml`               |
| macOS    | `~/Library/Application Support/devconcurrent/config.toml`  |
| Windows  | `%APPDATA%\devconcurrent\config\config.toml`               |

On Linux, `$XDG_CONFIG_HOME` is usually `~/.config`. Note that macOS does not
use `~/.config` here, and that Windows adds a `config` subdirectory.

If you are unsure, run any `devconcurrent` command with no config file in
place; the error names the exact path it looked for.

Place a file like this there:

```toml
#:schema https://raw.githubusercontent.com/paholg/devconcurrent/refs/heads/main/devconcurrent.schema.json

[projects.best_project]
path = "~/src/best"

[projects.second_project]
path = "~/src/second"
```

NOTE: The schema line at the top will let [tombi](https://tombi-toml.github.io/tombi/)
help you with this file. I highly recommend it!

For each project, you may also set `devcontainer`. This will merge with any
settings from a project's `devcontainer.json`, to allow you to have per-user
overrides. See <https://containers.dev/implementors/json_reference/>.

You also may specify `worktreeFolder` if you don't want to create worktrees in
devcontainer's default directory.

While we'll show snippets of configuring devconcurrent here, you can also view
the [full configuration options](CONFIGURATION.md).

## Layers

Like an ogre, `devconcurrent` has layers. You do not need to use all of its
features, and can mostly pick and choose as you wish.

Maybe you don't use devcontainers, and just want to use it to manage git
worktrees.

Maybe you don't care about worktrees, and just want a nice CLI for managing
devcontainers.

Maybe you don't care about either of those. You are happy having VSCode manage
your devcontainer, but you're excited about devcontainer's DNS server so you
can never think about ports again.

All of those use-cases are supported.

### CLI Quirks

It is expected that you generally want to operate on the workspace you are
within. For that reason, any CLI command that takes a workspace will default to
the one containing your current path if your working directory is in a workspace.

If you have multiple projects configured, then the project `dc` operates on is
determined as follows:

1. The `--project` flag
2. The `DC_PROJECT` environment variable
3. The current directory, if you're inside the workspace of any project
4. The first configured project

This way, if you have several projects you use frequently, you can set up
aliases. For example:

```sh
alias dcf='dc -p foopy'
alias dcb='dc -p bippity'
```

Then `dcb status` will _always_ show the status of `bippity`.

If this is confusing, please let me know!

### Workspaces

At its most basic, devconcurrent can manage git worktrees.

Run `dc up foo` to create the workspace `foo`. Run it with `-g` to also go (cd)
there.

Run `dc destroy foo` once you're done with it. Treat workspaces as cattle, not
pets. For me, each workspace is a branch is a single pull request. Full stop. If
creating workspaces isn't cheap, see [Tips](#tips) and make it cheap.

Run `dc status` to see all your workspaces and their current git dirtiness.

Run `dc go foo` to cd into `foo`'s directory. Because these paths tend to be
not super convenient, and because devconcurrent generates nice completions for
you, this ends up being pretty nice. I alias `dc go` to just `d`, so this would
be for me just `d f<TAB>`.

### Devcontainers

Currently, `devconcurrent` only supports a subset of devcontainer features. We
only support compose-based devcontainers, and we don't support
[features](https://containers.dev/implementors/features/). If you really need an
image-based devcontainer or feature support, open a ticket, but I would
encourage you to use compose, and to put whatever you need from the features
directly in your `Dockerfile`.

We provide some options via devcontainer's customizations section. In
`customizations.devconcurrent` (either in `devcontainer.json` or devconcurrent
config's `project.PROJECT_NAME.devcontainer.customizations.devconcurrent`), you
may set the following:

* `defaultExec` - What to run if you call `dc x` with no arguments. This might
  be going away, and we'll just run your default shell.
* `worktreeFolder` - Similar to the top-level `worktreeFolder`, this determines
  where worktrees are placed. It's duplicated here so that it can be configured
  in the project.
* `mountGit` - (default `true`) Whether the project's `.git` directory should
  be mounted into the devcontainer. Worktrees have a `.git` file that just
  contains the location of the project's true `.git` directory. Without this,
  `git` commands for non-root workspaces will not work in the devcontainer.
* `proxy` - This will be covered in the [DNS](#dns) and [Proxy](#proxy-and-https)
  sections.

This will enhance the commands we discussed above:

* Now, `dc up` will bring up the devcontainer as well, recreating an existing
  one, and running all lifecycle commands.
* Similarly, `dc destroy` will delete any containers and volumes (remember,
  workspaces are not pets -- if you care about losing data here, you're using
  them wrong).
* Finally, `dc status` will also show some docker information, and you can
  include the `--live` flag to use it as a monitor. You can also pass
  `--workspace` to see the containers within a workspace.
* You can also use `dc show` to show information about the current workspace;
  this can be useful if you want to include it in a shell prompt or similar.
  It has subcommands for the workspace name, forwarded ports, container IPs,
  proxied hostnames, and your [configured variables](#workspace-variables).

In addition, we introduce some new commands:

* `dc exec` or `dc x` will exec into your main container. I use this so often
  that I alias it to just `x`. This is how you'll run anything in the
  devcontainer.
* `dc fwd` or `dc f` will forward any ports specified in `forwardPorts` to the
  host. If ports are already being forwarded, `dc` will "move" them to the
  current workspace.
* `dc compose` or `dc c` will let you run any docker compose commands targeted
  at the workspace. For example, `dc c logs -f` to watch logs.

### DNS

While forwarding ports to containers is unarguably useful, it's quite annoying,
especially if you regularly have multiple workspaces active at a single time.
I've found myself very confused about why I didn't see what I expected only to
eventually realize I was forwarding ports to the wrong workspace! With a
different hostname for each workspace, this stops being an issue, but it does
take a bit of work.

With the setup here, say you have workspace `foo` and service `app`, with
a server in the container running at port `8080`, you can access it from the
host at `foo.app.test:8080`. At the same time, you can access `bar` at
`bar.app.test:8080`.

When enabled, devconcurrent runs a DNS server at default port `43770`. By
default, we use the `.test` TLD for all containers. You are free to configure
this, but be aware that `.test` is a [reserved](https://en.wikipedia.org/wiki/Special-use_domain_name)
domain, and real traffic will _never_ be served on it.

This guide will assume you're using these defaults, but you can configure them.
See port via `proxy.port`. Configuring the TLD is covered [below](#dns-configuration)

_NOTE:_ If you want this to work for containers launched with tools other than
`dc` (e.g. VS Code), you do need to set one label so devconcurrent knows what to
look for. When you run `dc up`, it sets this label for you. On your primary
service in `docker-compose.yml`, set:

```yaml
labels:
  - "com.paholg.devconcurrent.project=<PROJECT NAME>"
```

#### Linux

You'll need to configure your system to have devconcurrent handle DNS for the
`.test` TLD. How you do that depends on how your system manages DNS; here we
outline some possibilities, but your system may not be included.

The goal here: Have devconcurrent's DNS server running at `127.0.0.1:43770` handle
DNS requests for TLD `.test`.

Note: The following instructions assume you're on a system using `systemd`. If
you're unsure, you probably are.

**NixOs**
On `NixOs`, you can configure `resolved` as so:

```nix
services.resolved = {
  enable = true;
  settings.Resolve = {
    DNS = "127.0.0.1:43770";
    Domains = "~test";
  };
};
```

**systemd-resolved**
Run `systemctl is-active systemd-resolved`. If that reports `active`:

```sh
sudo mkdir -p /etc/systemd/resolved.conf.d
printf '[Resolve]\nDNS=127.0.0.1:43770\nDomains=~test\n' \
  | sudo tee /etc/systemd/resolved.conf.d/devconcurrent.conf
sudo systemctl restart systemd-resolved
```

**NetworkManager**
NOTE: These instructions should work, but I do not have a system with
NetworkManager. If you use them, please report back!

Run `systemctl is-active NetworkManager`. If that reports `active`:

NetworkManager only does conditional DNS forwarding when it's using its
`dnsmasq` backend, which is not the default. Check whether it's enabled:

```sh
NetworkManager --print-config | grep -A3 '\[main\]'
```

If `dns` isn't `dnsmasq`, enable it with a drop-in:

```sh
printf '[main]\ndns=dnsmasq\n' | sudo tee /etc/NetworkManager/conf.d/dns.conf
```

Then tell dnsmasq to forward `.test` to devconcurrent:

```sh
printf 'server=/test/127.0.0.1#43770\n' | sudo tee /etc/NetworkManager/dnsmasq.d/test.conf
```

Then restart NetworkManager:

```sh
sudo systemctl restart NetworkManager
```

#### MacOs

Docker Desktop does not provide container IPs to the host, which we need to be
able to direct traffic. I know of two tools to do this for you, but have not
personally used either:

* [Docker Mac Net Connect](https://github.com/chipmk/docker-mac-net-connect) is
  an open source tool for doing just this using wireguard.
* [OrbStack](https://orbstack.dev/) is a proprietary alternative to Docker
  Desktop that offers this via "Direct container access".

In addition, you'll need to configure your system to have devconcurrent handle
DNS for `.test`:

```sh
sudo mkdir -p /etc/resolver && \
  printf 'nameserver 127.0.0.1\nport 43770\n' | \
  sudo tee /etc/resolver/test
```

#### Windows

Windows has the same issue as MacOs, that docker IP addresses aren't available
to the Windows host. However, unlike MacOs, there doesn't seem to be a good
solution today. There is an [open PR](https://github.com/chipmk/docker-mac-net-connect/pull/37)
to Docker Mac Net Connect, but it's been sitting for a bit.

With the Linux instructions, this should work on WSL2, but I don't believe you'll
be able to access a container from e.g. a Windows browser.

If you use Windows and are interested on finding a solution, please feel free to
reach out to me!

#### Verification

Assuming you are using the default settings, have workspace `foo` and container
`app`, then after configuring the above, `dc up foo` will also bring up
devconcurrent's DNS server. Then `dig +short foo.app.test` should reveal the
container's IP address!

On MacOs, use `dscacheutil -q host -a name foo.app.test` instead. `dig` reads
`/etc/resolv.conf` and asks those servers directly; it never looks at
`/etc/resolver`, so it will report nothing here even when everything is working.

Better still, run `dc proxy status`. It checks every hostname and port you have
configured, one layer at a time, and tells you which one broke:

```text
WORKSPACE: foo

SERVICE  HOSTNAME      PORT      CONTAINER  SIDECAR  DNS  RESOLV  CONNECT  TLS  APP
app      foo.app.test  443→8080  ✓          ✓        ✓    ✓       ✓        ✓    200
db       foo.db.test   5432      ✓          -        ✓    ✗       ✗        -    -

  ✗ db 5432 · resolv: the system resolver doesn't know foo.db.test; .test isn't
    routed to 127.0.0.1:43770 (see the DNS section of the README)
  ✗ db 5432 · connect: nothing is listening on 172.18.0.3:5432
```

A `✗` under `DNS` means the proxy itself doesn't know the name; a `✗` under
`RESOLV` with `DNS` passing means the proxy knows it but your system isn't
asking the proxy — that's the setup above. It also reports whether the proxy is
running settings you have since changed, so you know when to re-run
`dc proxy up`, and whether your CA is actually in the system trust store —
which a passing `TLS` column does _not_ tell you, since that check trusts your
`caRoot` and nothing else. It exits non-zero if anything failed, and takes
`--json` if you want to script against it.

You can now reference containers by hostname. Ask for one with
`dc show hostname postgres`, or list them all with `dc show hostname`. The
service list comes from your compose configuration, so it doesn't depend on
what's currently running, and a typo'd service name is an error rather than a
hostname that resolves to nothing. There is also `dc show ip postgres`, but be
aware the IP can change on container re-creation.

Rather than calling that once per service, see
[workspace variables](#workspace-variables) to get them all in one go.

#### DNS Configuration

As already mentioned, you can customize the DNS port via `proxy.port` in
`config.toml`. In addition, you can customize the container hostnames if you
don't like the default via the devcontainer's
`customizations.devconcurrent.proxy.hostname`. This takes a
[handlebars](https://handlebarsjs.com/) template with the following variables
available:

* `root` (bool) - whether this is the root workspace
* `project` (string) - project name
* `workspace` (string) - workspace name
* `service` (string) - the compose service name

The default is `{{workspace}}.{{service}}.test`. If you wanted a different TLD
than `.test`, this is where you would set it.

A single service can override this via
`customizations.devconcurrent.proxy.services.<service>.hostname`, which takes the
same template. For example, to drop the service name from your main service's
hostname while leaving the rest alone:

```json
{
  "customizations": {
    "devconcurrent": {
      "proxy": {
        "services": {
          "app": { "hostname": "{{workspace}}.test" }
        }
      }
    }
  }
}
```

#### Workspace variables

You can configure variables for your project in `customizations.devconcurrent.env`:

```json
{
  "customizations": {
    "devconcurrent": {
      "env": {
        "APP_URL": "https://{{hostname 'app'}}",
        "DATABASE_URL": "postgres://postgres:postgres@{{hostname 'postgres'}}:5432/devel"
      }
    }
  }
}
```

The helper `{{hostname 'foo'}}` renders compose service `foo`'s hostname, and
the variables `root`, `project` and `workspace` are available.

`dc show env` prints them:

```text
VARIABLE      VALUE
APP_URL       https://feature3.app.test
DATABASE_URL  postgres://postgres:postgres@feature3.postgres.test:5432/db_feature3
```

If you'd like them to be automatically set when rendering your prompt, enable it
in in `config.toml`:

```toml
shell.exportEnv = true
```

[Shell Setup](#shell-setup) then registers a prompt hook alongside the `dc`
function, and your variables track whichever workspace you're standing in.mpt.

You can also do it by hand, which is all the hook does. `--export` takes the
shell to write for:

```sh
dc show env --export=bash
```

That relies on the `dc` shell function to apply the result. Without it, `eval`
the output yourself:

```sh
eval "$(devconcurrent show env --export=bash)"
```

Note that these variables are set in your shell, on the host.

If the hook seems to be doing nothing, run `dc show env` directly — the hook
stays quiet when you're outside a workspace, but a template that fails to render
is reported either way.

### Proxy and HTTPS

We've gotten pretty far, but there's one looming dark cloud: security. Now that
we're not using `localhost` to access our service, a lot of things might get
grumpy. Browsers don't like to visit non-localhost domains at plain `http`, and
some web frameworks don't like it either.

We can solve this with the final piece: A TLS-terminating proxy. Devconcurrent's
proxy operates in two modes; if you just want to do a port-map, then it operates
at [Layer 4](https://en.wikipedia.org/wiki/Transport_layer) and just proxies raw
bytes. But if you set `tls: true`, then it operates at
[Layer 7](https://en.wikipedia.org/wiki/Application_layer), listening for https
and setting headers.

#### HTTPS

There's a very handy tool, [mkcert](https://github.com/filosottile/mkcert),
and you can generate a certificate authority as easily as:

```sh
mkcert -install
```

If it's able, `mkcert` will also install this CA into your system and browser,
but if it's not, you may need to manually import it. This tends to be pretty
easy to do, and can be achieved via a few clicks in your browser's ui.

Then, see the path with `mkcert -CAROOT` and set it in devconcurrent's `config.toml`:

```toml
[proxy]
caRoot = "<PATH FROM `mkcert -CAROOT`>" 
```

#### Proxy settings

In `customizations.devconcurrent.proxy`, you'll want to set the following:

* `enable` (bool) - set to `true`
* `services` - a table of service names to a list of proxy options. Here's an
  example of a `devcontainer.json` file:

```json
{
  "name": "App Name",
  // OTHER OPTIONS HERE
  "customizations": {
    "devconcurrent": {
      "proxy": {
        "enable": true,
        "services": {
          "app": {
            "ports": [
              {
                "host": 443,
                "container": 8080,
                "tls": true
              },
              {
                "host": 80,
                "container": 8080
              }
            ]
          }
        }
      }
    }
  }
}
```

With that setup, a service in the `app` container running on port 8080 will be
accessible from the host at ports 80 and 443, with 443 doing full TLS
termination, so it should _just work_ in a browser. Also enabling port 80 lets
tools like `curl` work without needing `https`.

Note: You can still access the service at port 8080 from the host as well. The
port 80 entry there is just a convenience so you don't have to type ports ever.

## Tips

To make this tool work well, there are some important tips for how you configure
your devcontainers.

### Cache volumes

Devconcurrent is designed around creating and destroying workspaces frequently.
For this to be useful, it needs to be _fast_. For any dependencies or build
artifacts, I recommend you create `external` compose volumes, so they aren't
owned by any one workspace's compose project. To make this easy to manage, you
can auto-create them in your `initializeCommand`. For example, for a Rust
project I have in `docker-compose.yml`:

```yaml
services:
  cookit:
    # More settings here...
    volumes:
      - ..:/workspace:cached
      # Cache volumes:
      - nix-store:/nix
      - cargo-registry:/home/vscode/.cargo/registry
      - cargo-git:/home/vscode/.cargo/git
      - sccache:/home/vscode/.cache/sccache

volumes:
  pg_data:
  nix-store:
    name: cookit-nix-store
    external: true
  cargo-registry:
    name: cookit-cargo-registry
    external: true
  cargo-git:
    name: cookit-cargo-git
    external: true
  sccache:
    name: cookit-sccache
    external: true
```

and in my `initializeCommand`:

```sh
# Ensure external volumes exist
for vol in cookit-nix-store cookit-cargo-registry cookit-cargo-git cookit-sccache; do
    docker volume create "$vol" 2>/dev/null || true
done
```

This way users don't have to worry about manually creating these volumes, and no
workspace's compose tries to own them. The downside is that if a user is ever
completely done with your project, they will have to manually clean up the volumes.

### Ports

Do not specify static ports in `docker-compose.yml` -- any two workspaces _will_
conflict. You can specify `forwardPorts` in `devcontainer.json`, and `dc fwd`
will happily forward these.

If you _really_ need compose-forwarded ports, you can separate them.

For example, define your services without ports in `.devcontainer/docker-compose.yml`,
then in a root `docker-compose.yml` you can do:

```yaml
include:
  - path: .devcontainer/docker-compose.yml

services:
  postgres:
    ports:
      - "5432:5432"
  redis:
    ports:
      - "6379:6379"
```

Then devcontainer users have ports managed by `forwardPorts` and anyone running
`docker compose up` gets ports directly from docker.

## Glossary

We use a few terms repeatedly, and so want to make sure they have clear
definitions.

* `project` - Any devconcurrent-enabled git repository.
* `workspace` - A git worktree plus optional devcontainer. These the the main
  things that devconcurrent manages.
* `root workspace` - The workspace for the "main" worktree, as git calls it.

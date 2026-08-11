# Devconcurrent Configuration

You can configure devconcurrent in two places. First, you need at least a
`config.toml` in devconcurrent's config directory, which varies by platform.

Second, devconcurrent provides some options via devcontainer customizations.

We'll cover both here.

## Devconcurrent configuration

This file is located in the config directory reported by the
[`directories`](https://docs.rs/directories) crate, in `$XDG_CONFIG_HOME` or
your platform's equivalent. See the
[README](./README.md#configuration) for the full table.

First, if you use [tombi](https://tombi-toml.github.io/tombi/), then it's
recommended you start this config with the line

```toml
#:schema https://raw.githubusercontent.com/paholg/devconcurrent/refs/heads/main/devconcurrent.schema.json
```

Here are the options:

* `proxy` - configure the proxy.
  * `port` [default: 43770] - the port the proxy listens.
  * `caRoot` [optional] - The path given by `mkcert -CAROOT`.

* `projects.FOO` - configure project FOO.
  * `path` - the location of the git repository.
  * `worktreeFolder` - the directory where devconcurrent will place worktrees;
    defaults to the platform data directory: `$XDG_DATA_HOME/devconcurrent` on
    Linux, `~/Library/Application Support/devconcurrent` on macOS.
  * `devcontainer` - specify any of the options for [devcontainer.json](https://containers.dev/implementors/json_reference/).
    These will be merged with the project's `devcontainer.json` file, with
    arrays being merged, and settings from this file otherwise taking precedence.

## Devcontainer customization

In `devcontainer.json`, or `projects.FOO.devcontainer`, you can specify further
options. This allows you to configure devconcurrent for a project either in its
repository, or just for yourself, or some combination.

Here are the options that can go in `customizations.devconcurrent`:

* `defaultExec` - the default command run by `dc exec`. This is likely going away
  and we'll just run your default shell.
* `worktreeFolder` - the directory where devconcurrent will place worktrees;
  defaults to `$XDG_DATA_HOME/devconcurrent` or similar. This option is
  configurable redundantly so that non-devcontainer projects and set it and so
  that it can be configured in `devcontainer.json`.
* `mountGit` [default: `true`] - mount the project's git directory in workspace
  devcontainers. This allows git commands to work in containers in non-root
  workspaces.
* `proxy` - configure devconcurrent's proxy.
  * `enable` [default: `false`] - enable the proxy for this project.
  * `hostname` [default: `{{workspace}}.{{service}}.test`] - a [handlebars](https://handlebarsjs.com/)
    template to determine the hostname for containers.
  * `services.BAR` - configure the proxy for compose service `BAR`.
    * `ports` - configure the port-maps for this service. This is an array of objects.
      * `ip` [default: `0.0.0.0`] - the IP address the proxy will listen on.
      * `host` - the host port the proxy listens on
      * `container` - the container port the proxy forwards to
      * `tls` [default: `false`] - if `true`, the proxy will act as an http
        proxy, performing TLS termination so it can serve `https`. If `false`,
        the proxy just forwards raw bytes, acting as a simple port-forwarder.

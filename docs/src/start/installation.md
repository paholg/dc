# Installation

## Prerequisites

The only real requirement of `devconcurrent` is that you have a project in a git
repository.

If you want to use all of the features of `devconcurrent`, you'll need a modern
`docker`, and be interested in setting up a
[`devcontainer.json`](https://containers.dev/implementors/json_reference/) file
with docker-compose.

You'll also need to spend a small amount of effort to integrate
`devconcurrent`'s DNS server and CA with your system to be able to access your
container by URL.

## Binary Installation

Devconcurrent ships standard GitHub releases, is available as a brew tap, and
has a nix flake.

{{#tabs}}

{{#tab name="Homebrew"}}

```bash
brew install paholg/tap/devconcurrent
```

{{#endtab}}

{{#tab name="mise-en-place"}}

```bash
mise use -g github:paholg/devconcurrent
```

{{#endtab}}

{{#endtabs}}

See also [GitHub Releases](https://github.com/paholg/devconcurrent/releases/),
where there are release binaries and an install script.

## Shell Setup

Once installed, it's highly recommended you place the following in your shell
config file (`.bashrc` or similar).

{{#tabs}}

{{#tab name="Bash"}}

```bash
source <(COMPLETE=bash devconcurrent)
```

{{#endtab}}

{{#tab name="Fish"}}

```fish
COMPLETE=fish devconcurrent | source
```

{{#endtab}}

{{#tab name="Zsh"}}

```zsh
source <(COMPLETE=zsh devconcurrent)
```

{{#endtab}}

{{#endtabs}}

The rest of this document assumes you have run this, but if you have not,
replace `dc` commands with `devconcurrent`, and you'll lose some of the
functionality.

You are welcome to run just `COMPLETE=$SHELL devconcurrent` to see what it's
doing, but it's these three things:

1. It generates completions; we try to have very good shell completions for you.
2. It generates the `dc` function. In addition to letting you type less, this
   lets devconcurrent change your directory when you ask it to; it's what powers
   `dc go` and similar commands.
3. It generates a shell hook if you have `shell.exportEnv = true`. This powers
   having environment variables automatically set for you whenever you're in a
   workspace.

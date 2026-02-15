# dc - a worktree aware devcontainer manager

**NOTE:** This is brand new, experimental software. It is missing many features
of devcontainers, and likely has bugs. Use at your own risk!

Git worktrees allow you to have multiple branches checked out at the same time
in different directories.

Devcontainers can give you isolated development environments.

Combining these, you can have multiple, isolated development environments at the
same time. This allows you to easier prioritize incoming work without playing
the git stash/commit dance or worrying about which worktree is using what
dependency. It also lets you spin up devcontainers to cut AI agents loose
without interrupting your workflow.

## Overview

## Installation

After install, I recommend you place the following in your shell to get nice
completions:

**Bash**:
```bash
souce <(COMPLETE=bash dc)
```

**Elvish**:
```bash
eval (E:COMPLETE=elvish dc | slurp)
```

**Fish**:
```fish
COMPLETE=fish dc | source
```

**Zsh**:
```zsh
source <(COMLETE=zsh dc)
```

## Configuration

In order to give you a nice experience, we require a very simple confuration
file that just lists your projects.

In `~/.config/dc/config.toml` place a file like this:

```toml
[projects.best_project]
path = "~/src/best/"

[projects.second_project]
path = "~/src/second/"
```

We also add a customization to `devcontainer.json`. It is recommended that you
add a `dc` field with these properties, though none are required.

* `defaultExec` - The command to run on `dc exec` and `dc up --exec` if none is
 specified.
* `worktreeFolder` - The directory to place `dc`-generated worktrees. Defaults
  to `/tmp/`.
* `defaultCopyVolumes` - The volumes to copy with `dc copy` and `dc up --copy`
  if none are specified.
* `mountGit` [default `true`]- Whether to mount your project's git directory in
  workspace devcontainers. Git worktrees have a simple `.git` file that just
  holds the path to the real `.git` directory. If it's not available, then `git`
  commands won't work. This flag ensures it's available.

We also make use of `forwardPorts` from the standard devcontainer configuration.

## Detailed Usage

## Devcontainer Tips

To make this tool work well, there are some important tips for how you configure
your devcontainers.

### Ports

### Configuration and Caches

One issue with spinning up short-lived devcontainers is that you have to
configure them and repopulate caches (such as dependency directories) every
time. Gross!

To make using `dc` fast and breezy, you'll want to make heavy use of volumes and
mounts.

For anything that can be checked into git, do that, and then you can symlink it
into place in `postCreate` or `postStart`. This ends up being pretty nice -- if
you change any settings, they'll be reflected in git.

The flipside is that it only works for configuration that you're happy to share,
so no secrets or developer-specific settings.

TODO



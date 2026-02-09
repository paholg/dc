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
* `worktreeFolter` - The directory to place `dc`-generated worktrees. Defaults
  to `/tmp/`.
* `forwardPort` - The default port to use for `dc fwd`.
* `defaultCopyVolumes` - The volumes to copy with `dc copy` and `dc up --copy`
  if none are specified.

## Detailed Usage

## Devcontainer Tips

To make this tool work well, there are some important tips for how you configure
your devcontainers.

### Ports

Do not forward hard-coded ports to your containers; this will prevent `dc` from
bringing up more than one at a time.

Instead, either allow the OS to provide a port, and track it, or use `dc fwd` to
forward ports to workspaces of your choice. This keeps you flexible -- you can
run `dc fwd` at any time and change which container you forward too.

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



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

## Detailed Usage

## Devcontainer Tips

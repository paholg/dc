# Workspaces

A workspace is an instance of a project; essentially a git worktree plus an
optional devcontainer. It should be a self-contained development environment, at
least up to shared caches and the like.

We refer to the original workspace as the root workspace; its name is always the
same as the project name, and no other workspaces can be created with that name.

I recommend that you do no work in the root workspace. New workspaces branch
from the root. You can keep them clean by keeping the root always on the `main`
branch.

I also recommend that workspaces other than the root are short-lived. For me,
one workspace is one pull request, and then I destroy it. This helps keep me
organized; I always have a 1-1 relationship between workspaces, branches, and
pull requests.

## Commands

All of `devconcurrent`'s commands pick a project in the following priority:

1. A top-level `-p/--project` flag.
2. The `DEVCONCURRENT_PROJECT` environment variable.
3. Your current working directory.
4. Your first configured project.

This way, if you have several projects, you can define aliases like
`alias dcc="dc -p cookit"` to always target that project, or you can just `cd`
into the project you want to work on first.

Here are some commands for workspaces; see the
[CLI Reference](../reference/cli.md) or `dc --help` for the full commands and
options.

### `dc up NAME`

Create a new workspace. Use `-g/--go` to also go to it.

When a workspace is created, it checks out a branch named by the `worktree.branch`
template in your `config.toml`, which defaults to the workspace name. If that
branch does not exist, it is cut from the root project. Use `-b/--branch` to use
a different branch name or `-d/--detach` for no branch.

To prefix your branches, set the template globally or per project:

```toml,filename=config.toml
[worktree]
branch = "plg/{{ workspace }}"

[projects.my-app]
path = "~/src/my-app"
branch = "{{ project }}/{{ workspace }}"
```

With this, `dc up foo` creates worktree `foo` on branch `plg/foo`, or
`my-app/foo` in `my-app`. Templates get `project` and `workspace`; see the
[config.toml reference](../reference/config-toml.md).

### `dc go NAME`

Go to a workspace. Because devconcurrent's completions are so nice, I alias this
to just `d`, so I can do e.g. `d f<TAB>` to go to the `floopydoopy` workspace.

### `dc destroy NAME`

Destroy a workspace. The names is optional; if blank, targets the current
workspace, and moves you to the root after completion.

### `dc status`

Show all of your workspaces in a table, with `git status` information. The
symbols here are modeled after
[starship](https://starship.rs/config/#git-status).

### `dc show`

Use `dc show workspace` to see the current workspace, or exit 1 if not in a
workspace. This can be used for prompt-integration and the like.

# Devcontainers

A [devcontainer](https://containers.dev/) is essentially a
[docker compose](https://docs.docker.com/compose/) environment with some extra
tooling built around it.

It gives you some services you can configure, and a primary service, which is
where you are intended to work. However, `devconcurrent` understands that you
may not always want to work in the container, and aims to make your life easy
and painless whether you're inside or outside of it.

Currently, we do not support the full devcontainer spec; we only support
compose-based containers, and we don't support
[features](https://containers.dev/implementors/features/). If you need either of
these, please open an issue.

## Configuration

This is covered primarily in
[Porject Devcontainer Setup](../project/devcontainer.md), but I want to point
out here that you don't need the project to explicitly support `devconcurrent`,
or even devcontainers at all. You can put any `devcontainer.json` overrids in
your `config.toml`.

## Commands

With devcontainers, some of the commands mentioned in
[Workspaces](./workspaces.md) are enchriched, and others are added.

### `dc up NAME`

When creating a workspace, we launch the devcontainer, running all lifecycle
commands. You can use this on an existing workspace to recreate it. If I would
like my container updated with upstream changes, I will sometimes run

```bash
git rebase origin/main && dc up
```

### `dc destroy NAME`

This will always remove the containers and any volumes.

### `dc status`

This command is enriched with information about the containers; status, memory
used, etc.

Use `-l/--live` to include CPU and keep it running, and `-w/--workspace NAME` to
instead view a single workspace by container.

### `dc show`

There are many more options here; run `dc show --help` to see them all. These
all tend to just output short strings for use in prompts and the like.

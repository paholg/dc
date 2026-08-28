# Introduction

Welcome to `devconcurrent`, your new development environment manager!

## Organization

This document is largely divided into two sections;
[User Guide](./user/introduction.md) for how to setup your machine and personal
configuration and [Project Guide](./project/introduction.md) for how to
configure a project.

In both sections, we try to dive into how `devconcurrent` works a bit, so it may
be valuable for you to skim both, even if you're only setting up one of the two.

## Value Proposition

With git worktrees, you can work on multiple branches of a project in different
directories at the same time.

With devcontainers, you _can_ have isolated development environments, but there
isn't great tooling outside of VS Code, and the reality of ports make this
harder than it should be.

Enter `devconcurrent`, a tool to easily manage worktrees + devcontainers; it can
bring them up, take them down, exec into them, etc all with simple CLI commands.

With `dc up foo` you have a brand new worktree named `foo`, running its
devcontainer, ready for you! Once you're done, just `dc destroy foo`.

On top of that, devconcurrent gives you a local DNS server and TLS-terminating
proxy. With a little bit of setup, if you have some web app `app` and are
working on `feature3`, you can view it in your browser at
`https://feature3.app.test`!

# devconcurrent

Give every branch its own environment with easy access when you need it.

[![Book](https://img.shields.io/badge/book-devconcurrent.paholg.com-blue?logo=mdbook)](https://devconcurrent.paholg.com)
[![Release](https://img.shields.io/github/v/release/paholg/devconcurrent)](https://github.com/paholg/devconcurrent/releases/latest)
[![CI](https://github.com/paholg/devconcurrent/actions/workflows/ci.yml/badge.svg)](https://github.com/paholg/devconcurrent/actions/workflows/ci.yml)

**NOTE:** This is new, experimental software. Use at your own risk. It's still
undergoing rapid breaking changes; expect this to slow down once above version
0.1.0, and to mostly stop at 1.0.0. Expect a CHANGELOG starting with version
0.1.0.

With devconcurrent, you get easy setup and full control of multiple development
environments at the same time via [git worktrees](https://git-scm.com/docs/git-worktree)
and [devcontainers](https://containers.dev/). The devcontainer management works
whether or not you use devcontainers with VS Code or similar tools.

The short pitch:

* Never `git stash` again when interrupted mid-task.
* Never worry about database migrations messing up your DB state.
* Run as many AI agents as you want without worrying about them getting in yours
  or eachother's way.
* Easily access your containers by URL, and never worry about port conflicts again.

![short demo of use](docs/src/demos/demo.gif)

---

Run `dc up foo` to get:

* A new worktree, `foo`, with its own, isolated devcontainer stack (hereafter
  referred to as a "workspace").
* A DNS server and HTTP proxy, so you can view your app at
  `https://foo.app.test` and reach your database at `foo.postgres.test`.
* Autopopulated environment variables, so just being in the directory is enough
  to have `DATABASE_HOST=foo.postgres.test` set.

Enter the devcontainer with `dc x`, inspect your workspaces with `dc status` and
clean them up with `dc destroy`. Work on the host or inside the container,
whichever you prefer.

---

<p align="center">
  <a href="https://devconcurrent.paholg.com">
    <img src="https://img.shields.io/badge/Get_Started_Here-blue?style=for-the-badge&logo=mdbook" alt="Get Started Here">
  </a>
</p>

---

# Documenatation updates

We're rewriting documentation, so changes in the meantime will be documented
here.

## Matching the container user to you

`mountGit` shares one `.git` directory between the host and the container. If
the container's user has a different uid than yours, everything it writes there
is owned by a user you aren't, and host-side `git` starts refusing to work on
its own repository.

To avoid that, on Linux `dc up` remaps the container user to your uid and gid,
per the devcontainer spec's `updateRemoteUserUID` (on by default). It works the
way the reference implementation does: `dc` builds a small image layer on top
of your service's image that rewrites the user's `/etc/passwd` entry and chowns
their home directory. The layer is cached, so it costs nothing after the first
build, and `dc destroy` removes the derived image along with everything else.

Two consequences worth knowing:

* Because it happens at build time, nothing mounted from the host is ever
  chowned — only the image's own files.
* If your uid or gid is already taken inside the image by a different user or
  group, that half of the remap is skipped rather than forced.

Set `"updateRemoteUserUID": false` in `devcontainer.json` to turn it off. It is
Linux-only; Docker Desktop on macOS already reconciles ownership itself.

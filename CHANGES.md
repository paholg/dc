# Documenatation updates

We're rewriting documentation, so changes in the meantime will be documented
here.

NOTE: Document the dangers of `mountGit` -- a container can write to it, which
could result in host execution.

## Trusting the CA is built in

mkcert is no longer needed (this supersedes the trust instructions in the
HTTPS section below). Two new commands do its job:

* `dc proxy trust` — generates the CA if needed, then installs it into the
  system trust store (via sudo) and, when `certutil` from the NSS tools is
  available, into the browsers' own stores: Firefox, and Chromium on Linux.
  Without `certutil` the browser stores are skipped with a notice naming the
  package to install (`libnss3-tools`, `nss-tools`, `nss`, or `brew install
  nss`, depending on the platform).
* `dc proxy untrust` — removes the CA from all of those stores, then deletes
  its certificate and key files; the next `dc proxy up` or `dc proxy trust`
  generates a fresh CA. It also removes the entries a root installed with the
  old `CAROOT=... mkcert -install` instructions left behind, so switching
  over needs no cleanup.

The `trust` row of `dc proxy status` and the TLD-change error now point at
these commands. `dc show ca-root` still prints the CA directory for manual
imports. On macOS 15 and later, the system may additionally ask you to
confirm the change in a security dialog.

On NixOS there is no writable system store; instead set

```nix
security.pki.certificateFiles = [ "<ca-root>/rootCA.pem" ];
```

in your system configuration, with the directory printed by `dc show ca-root`.

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

## HTTPS: devconcurrent now manages its own CA

The mkcert-CAROOT integration is gone. devconcurrent generates and manages its
own certificate authority; mkcert is only used (optionally) to install trust.

How it works:

* On first `dc proxy up`, a root CA is generated in `proxy.caRoot`, which now
  defaults to `ca` under the platform data directory
  (`~/.local/share/devconcurrent/ca` on Linux,
  `~/Library/Application Support/devconcurrent/ca` on macOS). `caRoot` no
  longer needs to be set at all, and there is no longer an http-only mode —
  TLS is always available.
* The root is X.509 name-constrained to the TLDs in the new `proxy.tlds`
  config option (default `["test"]`): trusting it only extends trust for those
  suffixes — nothing chained to it can ever vouch for a real domain. Each
  entry covers itself and all of its subdomains, so any TLD used in a hostname
  template must be listed there or browsers will reject its certificate.
* The one manual step is trusting the root:

  ```sh
  CAROOT=$(devconcurrent show ca-root) mkcert -install
  ```

  The new `dc show ca-root` command prints the CA root directory — generating
  the CA first if it isn't there yet, so mkcert never gets handed an empty
  CAROOT (it would generate its own, unusable root there). The `trust` row of
  `dc proxy status` reminds you of the command until it's done. If mkcert
  can't install the root everywhere, import `rootCA.pem` manually.
* The root's key never enters a container. At `dc proxy up`, the CLI uses it
  on the host to mint an intermediate CA — valid 30 days, carrying the same
  TLD constraints — and uploads only that into the proxy container, where it
  dies with the container. Even a fully compromised proxy can't sign
  certificates for anything outside your TLDs. Sidecars serve the leaf +
  intermediate chain and never hold a CA key.
* Renewal is automatic: any `dc` command that finds the intermediate within a
  week of expiring recreates the proxy with a fresh one. `dc proxy status`
  gained an `intermediate` row showing the current expiry.
* Changing `proxy.tlds` doesn't silently replace the root: the old one would
  stay trusted with no way to untrust it (`mkcert -uninstall` identifies the
  cert by reading CAROOT, so it needs the file to still exist). Instead `dc`
  errors with the replacement steps — untrust the old root, delete its files,
  rerun, trust the new one.
* Only a devconcurrent-generated root works as `caRoot`; `dc` rejects
  anything else with guidance. In particular, a root generated by
  `mkcert -install` itself carries `pathlen:0`, which forbids any
  intermediate beneath it. Leave `caRoot` at its default (or any fresh
  directory) and let devconcurrent generate the root.

Configuration reference changes:

* `proxy.caRoot` — no longer "the path from `mkcert -CAROOT`". Now: the
  directory holding the root CA the proxy's certificates chain to; defaults to
  `ca` under the platform data directory; generated there if missing.
* `proxy.tlds` [new, default `["test"]`] — DNS suffixes the proxy may serve
  TLS for; both the root and intermediate CA are name-constrained to these.

## Default worktree folder moved

The default `worktreeFolder` is now `workspaces/<project>` under the platform
data directory (previously `<project>` directly under it), keeping generated
worktrees apart from other state like the CA directory. There is no migration:
worktrees created under the old default are not found at the new one.

## When two services want the same hostname

The default hostname template is `{{workspace}}.{{service}}.test`, so a name is
unique per workspace and service — but it carries no project. Two projects with
a workspace of the same name would render the same hostname.

That particular case can't reach the proxy: `dc up` refuses to run compose
against a project name another workspace has already claimed, because the
devcontainer convention derives it from the worktree folder name alone. So two
workspaces cannot share a name even across projects, and the default template
cannot collide.

What can still collide:

* A `customizations.devconcurrent.proxy.hostname` template that drops
  `{{workspace}}` or `{{service}}` — `{{project}}.test`, say, which every
  service of every workspace in the project renders identically.
* A per-service `hostname` override that lands on a name another service
  already renders.
* Containers brought up outside `dc up`, such as by VS Code, which never pass
  the project-name check above.

When it happens, the proxy registers one of the colliding services and drops
the rest from DNS, logging a warning. Which one it keeps is not defined, and it
can change as containers stop and start — so treat a collision as something to
fix rather than something to resolve in your favor. You'll see it in
`dc proxy status`, which marks the losing rows and names the other claimant,
and in the proxy container's logs.

# HTTPS and the Proxy

<!-- OUTLINE — replace with prose.
Source: README-old.md "Proxy and HTTPS" (L496–574).

- Motivation: browsers/frameworks dislike non-localhost http.
- Two modes: L4 raw-byte port map vs L7 TLS termination (`tls: true`); one
  short paragraph, not a networking lesson.
- Certificates: mkcert, `mkcert -install`, manual browser import fallback,
  `proxy.caRoot` in config.toml.
- Enabling: `proxy.enable`, `services.<name>.ports` example (host 443→8080
  tls, host 80→8080 plain); container port still reachable directly.
- Proxy lifecycle (GAP — barely documented today):
  - `dc proxy up` / `dc proxy down` / `dc proxy status`.
  - When you must re-run `dc proxy up` (settings changes; status tells you).
  - Architecture: what actually runs (sidecar container? the SIDECAR status
    column implies one); one proxy across multiple projects/workspaces.
  - Which workspace's config the proxy reads (`--workspace` flag on proxy up).
- Headers set in L7 mode (GAP: which ones — X-Forwarded-*?).
-->

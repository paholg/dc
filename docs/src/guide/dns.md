# DNS

<!-- OUTLINE — replace with prose.
Source: README-old.md "DNS" intro (L226–256).

- Motivation: per-workspace hostnames instead of port juggling;
  `foo.app.test:8080` vs `bar.app.test:8080`.
- How it works: devconcurrent runs a DNS server on 127.0.0.1:43770 answering
  for `.test`; your system forwards `.test` queries to it; answers are
  container IPs. GAP: when does the server start/stop (with `dc up`? with
  `dc proxy up`? is it the proxy sidecar?) — undocumented today.
- `.test` is a reserved TLD; safe by design; configurable (link to Templates
  reference for hostname/TLD, config reference for port).
- The compose label for containers launched outside `dc` (VS Code case).
- Hand off to per-platform setup subchapters; then Verification.
-->

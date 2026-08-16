# DNS: Verification

<!-- OUTLINE — replace with prose.
Source: README-old.md "Verification" (L357–400).

- `dc proxy status` first — it's the real tool. Sample output table.
- Reading the columns: DNS vs RESOLV distinction; stale-settings detection;
  the TLS-column-doesn't-check-trust-store caveat; `--json`; non-zero exit.
- Manual checks: `dig +short` on Linux, `dscacheutil` on macOS (and why dig
  lies there).
- `dc show hostname` / `dc show ip`; hostname list comes from compose config,
  typos error out; IPs change on recreation.
- Pointer to Workspace Variables for getting all of these at once.
-->

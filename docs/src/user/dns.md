# DNS

This is where the true magic starts to happen, allowing you to access each
container by a unique hostname.

By default, when its proxy is enabled, `devconcurrent` runs a DNS server on port
43770 that listens for the TLD `.test`. You can configure either of these, but I
strongly recommend you limit `devconcurrent` to TLDs that cannot serve real
traffic.

There are several reserved TLDs, and you can read about them on
[Wikipedia](https://en.wikipedia.org/wiki/Top-level_domain#Reserved_domains).

In order for this to work, you need to point your system to use `devconcurrent`
for DNS for just `.test`.

See the platform-specific instructions for how to do this.

To check the health of all stages involved in the proxy, run `dc proxy status`.
This should help point out any gaps in your setup.

## System Setup

In order for `devconcurrent` to be able to resolve your containers' hostnames,
your system needs to defer to it for the `.test` TLD. Because we're just using
it for a reserved TLD that cannot serve real traffic, it should not affect the
rest of your DNS queries.

See the instructions for your operating system, then head to
[Verification](./dns/verification.md)

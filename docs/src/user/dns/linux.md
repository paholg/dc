# DNS: Linux

On Linux, there are various different methods your system may be handling DNS;
we cover the most common ones here.

The instructions below aim to not just tell you how to set up the DNS, but to
help find out which applies to you.

## systemd-resolvd

Run `systemctl is-active systemd-resolvd`. If that reports `active`:

```sh
sudo mkdir -p /etc/systemd/resolved.conf.d
printf '[Resolve]\nDNS=127.0.0.1:43770\nDomains=~test\n' \
  | sudo tee /etc/systemd/resolved.conf.d/devconcurrent.conf
sudo systemctl restart systemd-resolved
```

## NetworkManager

NOTE: These instructions should work, but I do not have a system that uses
NetworkManager for DNS. If you use them, please report back!

Run `systemctl is-active NetworkManager`. If that reports `active`:

NetworkManager only does conditional DNS forwarding when it's using its
`dnsmasq` backend, which is not the default. Check whether it's enabled:

```sh
NetworkManager --print-config | grep -A3 '\[main\]'
```

If `dns` isn't `dnsmasq`, enable it with a drop-in:

```sh
printf '[main]\ndns=dnsmasq\n' | sudo tee /etc/NetworkManager/conf.d/dns.conf
```

Then tell dnsmasq to forward `.test` to devconcurrent:

```sh
printf 'server=/test/127.0.0.1#43770\n' \
  | sudo tee /etc/NetworkManager/dnsmasq.d/test.conf
```

Then restart NetworkManager:

```sh
sudo systemctl restart NetworkManager
```

## NixOs

On `NixOs`, you can configure `resolved` declaratively:

```nix
services.resolved = {
  enable = true;
  settings.Resolve = {
    DNS = "127.0.0.1:43770";
    Domains = "~test";
  };
};
```

## Other

If none of this works for you, please file an issue.

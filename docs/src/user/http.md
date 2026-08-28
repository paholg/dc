# HTTP

If you've gotten this far, you're in pretty good shape; you have isolated
workspaces, and you can access each one by a unique hostname.

There are really only two issues that remain:

1. You still have to remember what ports things listen on!
2. TLS

## Ports

Fixing 1 is trivial for http servers; just set `httpProxyPort` to the port a
service listens on:

```json,filename=devcontainer.json
{
  ...,
  "customizations": {
    "devconcurrent": {
      "proxy": {
        "enable": true,
        "services": {
          "app": {
            "httpProxyPort": 8080
          },
          "jaeger": {
            "httpProxyPort": 16686
          }
        }
      }
    }
  }
}
```

Then, on `dc up`, `devconcurrent` will automatically launch an HTTP proxy on
port 80 for those services, so you can access them by hostname, no port.

## TLS

What I mean by "TLS", is that as soon as you direct a browser to something other
than `localhost`, it starts to get grumpy if its not a secure connection.
Similarly, many web frameworks will get upset if they serve traffic on what they
deem to be non-local hostnames without `https`.

The good news is that we can fix that; `devconcurrent` can generate a CA and we
just need to tell your system or browser to trust it.

This sounds scary, but `devconcurrent`'s CA is only valid for its configured
TLDs, so as long as those don't include TLDs that can serve real traffic (you
didn't put `com` in there or something, _right_?), there's not much to worry
about.

Run `dc proxy trust` to do just that.

If this does not work for you, or if you'd prefer to trust the CA yourself, you
can find it at `dc show ca-root`. This points to a directory containing the file
`rootCA.pem`. You can import that into your browser's certificates via its UI,
or add it on NixOs with:

```nix
security.pki.certificateFiles = [ ./path/to/rootCA.pem ]
```

Once that's done, everything should just work. The proxy also serves traffic on
port 443, and if you run `dc proxy status` while running a service in every
container with a configured `httpProxyPort`, you should see only green.

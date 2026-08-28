# DNS: Verification

For some project with at least one devcontainer service and the proxy enabled:

```json,filename=devcontainer.json
{
  ...,
  "customizations": {
    "devconcurrent": {
      "proxy": {
        "enable": true,
        }
      }
    }
  }
}
```

Run `dc up`. This will start the workspace containers as well as the DNS server.

Run `dc proxy status`; you should see green checks for `DNS` and `RESOLV` for
your service(s). If `DNS` is red, then there's something wrong with
`devconcurrent`, and if `RESOLV` is red, then devconcurrent is not getting
traffic from your OS.

If your services are listening on any ports, you should be able to reach them on
those ports using the hostname; for an http service at `setup.app.test` that
listens on port 3000, `curl setup.app.test:3000` should work.

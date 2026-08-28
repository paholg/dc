# DNS: macOS

Docker Desktop does not provide container IPs to the host, which we need to be
able to direct traffic. Here are two tools that can do this:

- [Docker Mac Net Connect](https://github.com/chipmk/docker-mac-net-connect) is
  an open source tool for doing just this using wireguard.
- [OrbStack](https://orbstack.dev/) is a proprietary alternative to Docker
  Desktop that offers this via "Direct container access".

Once that's resolved, you'll need to configure your system to have devconcurrent
handle DNS for `.test`:

```sh
sudo mkdir -p /etc/resolver && \
  printf 'nameserver 127.0.0.1\nport 43770\n' | \
  sudo tee /etc/resolver/test
```

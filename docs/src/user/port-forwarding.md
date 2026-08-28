# Port Forwarding

NOTE: If you're planning on continuing to the DNS section, you can skip this.

In `devcontainer.json`, there's a section for `forwardPorts`; this is what we
use for forwarding ports. However, we do not pay attention to `portsAttributes`;
forwarding ports with `devconcurrent` is an explicit action, so the semantics
are a bit different than with a long-lived editor.

When ports are forwarded, services in containers see requests from the host as
coming from localhost. This is accomplished by creating two thin docker
sidecars; one inside the target's network namespace, and one that listens to the
port on the host.

Managing ports is cumbersome and error-prone; it's easy to forget which
workspace has the ports at any moment. Instead, in the next sections, we'll
cover accessing the containers by hostname so you never have to do this.

## Commands

### `dc fwd`

Start forwarding ports for the current workspace (or one given by
\`-w/--workspace).

If any ports are already forwarded by `devconcurrent`, they are moved to this
workspace. If any are already in use by something else, we log a warning and
forward what we can.

This allows you to "move" ports to the current workspace with a simple `dc f`.

### `dc fwd stop`

Stop forwarding ports.

### `dc show ports`

Show what ports are being forwarded to the current workspace; this can be useful
in a shell prompt if you'd like a reminder if you're currently forwarding here.

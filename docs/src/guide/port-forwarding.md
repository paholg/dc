# Port Forwarding

<!-- OUTLINE — replace with prose.
Source: README-old.md `dc fwd` bullet (L218–221) + "Ports" tip (L632–655).

- `dc fwd` / `dc f`: forwards `forwardPorts` from devcontainer.json.
- The "move" semantics: forwarding while another workspace holds the ports
  moves them. Why: only one workspace can own a host port.
- The wrong-workspace confusion story (L228–233) fits here as motivation for
  the DNS chapter — end with "or stop forwarding ports entirely: next chapter".
- GAPS:
  - How forwarding is implemented / how long it lasts / how to stop it.
  - `dc show ports` to see what's currently forwarded.
- Note: repo-side advice (don't put static ports in compose) lives in
  Project Setup → Ports; link there.
-->

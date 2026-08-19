# Demos

Terminal demos, using [VHS][] and the [dcex][] example repo, rendered to
`docs/src/demos/`.

These are slow and not fully deterministic, so we record them manually.

## Recording

```bash
demos/record.sh demo.tape
```

[`record.sh`](record.sh) builds `devconcurrent`, runs
[`setup-dcex.sh`](setup-dcex.sh), and hands the tape to `vhs` from the repo
root.

Start each tape with:

```text
Output docs/src/demos/<NAME>.gif
Source demos/lib/setup.tape
```

[`shell.sh`](shell.sh) drops you into that same environment without recording,
for working out what a tape should say before you write it.

[dcex]: https://github.com/paholg/dcex
[VHS]: https://github.com/charmbracelet/vhs

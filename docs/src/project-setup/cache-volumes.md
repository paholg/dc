# Cache Volumes

<!-- OUTLINE — replace with prose.
Source: README-old.md "Cache volumes" (L579–630).

- Why: destroy/create must be fast; shared external volumes for deps and build
  artifacts.
- The Rust example: compose volumes block + initializeCommand loop.
- Trade-off: manual cleanup when done with a project forever.
- GAP: any concurrency caveats — two workspaces writing one cache volume at
  once (cargo/sccache handle locking; not everything does).
-->

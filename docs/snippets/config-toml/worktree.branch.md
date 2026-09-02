**Example:**

```toml,filename=config.toml
[worktree]
# `dc up foo` checks out `plg/foo`.
branch = "plg/{{ workspace }}"
```

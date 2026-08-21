**Example:**

If a project uses devcontainers, but not devconcurrent, you might configure it
here like this:

```toml
[projects.my-app]
path = "~/src/my-app"
# Per-user devcontainer overrides, merged into the repo's devcontainer.json:
devcontainer.customizations.devconcurrent = {
  env = {
    APP_URL = "https://{{ hostname 'app' }}",
    DATABASE_HOST = "{{ hostname 'mysql' }}"
  },
  proxy = {
    enable = true,
    services.app.containerPort = 8080,
  },
}
```

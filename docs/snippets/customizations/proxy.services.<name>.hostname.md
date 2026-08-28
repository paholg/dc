**Example:**

You may want a simpler hostname for your primary service.

```json,filename=devcontainer.json
{
  ...,
  "customizations": {
    "devconcurrent": {
      "proxy": {
        "enable": true,
        "services": {
          "app": {
            "httpProxyPort": 3000,
            "hostname": "{{workspace}.test"
          }
        }
      }
    }
  }
}
```

# DNS: Windows Setup

Unfortunately, we don't yet have a solution for Windows. Like Mac, container IPs
are not reachable from the Windows host. Unlike Mac, there are no existing tools
to help with this.

You should be able to do everything in WSL2 using the Linux instructions, but
you won't be able to resolve your container hostnames from the Windows host.

Stay tuned, we'll try to have a fix for this soon!

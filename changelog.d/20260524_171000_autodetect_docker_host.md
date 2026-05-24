---
bump: patch
---

### Fixed
- Autodetect `tcp://host.docker.internal:2375` for MCP subprocesses when `DOCKER_HOST` is scrubbed and `/var/run/docker.sock` is absent.

# spinmux v0.1.0: Autonomous Multi-Service Cockpit

We are proud to announce the initial release of **spinmux** (v0.1.0), the high-craft terminal cockpit for local multi-service development.

### Highlights
- 🔍 **Service Auto-Discovery:** Seamlessly extracts dev services from `package.json`, `Procfile`, and `docker-compose.yml`.
- 🌐 **Virtual Localhost Reverse Proxy:** Instant dynamic subdomain routing (`http://<subdomain>.localhost:<port>`) using `hyper`.
- 🛡️ **Zero-Orphan Process Supervision:** Encapsulates process trees in Windows Job Objects (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) and POSIX process groups.
- ⚡ **Electric Cyan/Violet TUI:** Sub-15ms latency Ratatui interface with interactive process control and split-screen log streaming.
- 📜 **Lock-Free Ring-Buffers:** In-memory, high-throughput stdout/stderr log capture.

### Assets Included
- `spinmux.exe` (Windows x86_64 standalone executable)
- `spinmux-v0.1.0-x86_64-pc-windows-msvc.zip` (packaged with docs & license)

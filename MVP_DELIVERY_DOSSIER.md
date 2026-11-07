# spinmux — MVP Delivery Dossier
**Version:** v0.1.0  
**Release Date:** 2026-11-07  
**Project Location:** `d:\spinmux`  
**Remote Repository:** `https://github.com/shenald-dev/spinmux`  
**License:** MIT License  

---

## 1. Executive Summary & Release Metadata

`spinmux` is an autonomous multi-service development cockpit designed to streamline local microservices and full-stack web development. It eliminates terminal sprawl, orphan background processes, port collisions, and DNS hacks by combining:
1. **Intelligent Service Auto-Discovery**: Automatically recognizes and parses services from `package.json` scripts, `Procfile`, and `docker-compose.yml`.
2. **Deterministic Zero-Orphan Process Supervision**: Enforces child process encapsulation using native Win32 Job Objects (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) on Windows and POSIX process groups with `killpg` on Unix.
3. **Ephemeral Virtual Localhost Reverse Proxy**: Transparently routes requests like `http://api.localhost:8080` to dynamically allocated local ports via Hyper 1.x without `/etc/hosts` modifications (RFC 6761 compliant).
4. **Sub-15ms Electric Cyan/Violet TUI**: Split-screen dashboard built with Ratatui and Crossterm for live log streaming, auto-scroll locking, regex search, and interactive service control hotkeys.
5. **High-Throughput Lock-Free Ring-Buffers**: In-memory circular log buffers capturing stdout and stderr streams asynchronously.

### Release Artifacts
- **Binary**: `target/release/spinmux.exe` (Windows x86_64 standalone release binary)
- **Archive**: `spinmux-v0.1.0-x86_64-pc-windows-msvc.zip` (with SHA-256 checksum)
- **Git Release Tag**: `v0.1.0`
- **GitHub Release**: `https://github.com/shenald-dev/spinmux/releases/tag/v0.1.0`

---

## 2. System Architecture & Core Capabilities (R1–R5)

### Requirement R1: Service Auto-Discovery & Process Supervisor
- **Discovery Engine (`src/discovery/`)**:
  - Parses `package.json` npm scripts (prioritizing `dev`, `start`, `serve`, `api`, `worker`, etc., while ignoring `test` and `build`).
  - Parses Foreman/Honcho `Procfile` definitions with variable interpolation and comments.
  - Parses `docker-compose.yml` service definitions, commands, and port mappings.
  - Resolves service naming conflicts and sanitizes subdomains according to RFC 1123.
- **Process Supervisor (`src/supervisor/`)**:
  - Encapsulates child processes in Win32 Job Objects configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
  - Guarantees termination of all child and grandchild processes upon exit or parent crash (Zero Orphans).
  - Automatically allocates ephemeral ports (`127.0.0.1:0`) and injects `PORT=<port>` and `SPINMUX=1` into child environments.
- **Lock-Free Ring-Buffer (`src/ringbuf/`)**:
  - Fixed-capacity circular memory buffer storing up to 10,000 log lines with timestamp tagging and stream separation (`stdout`/`stderr`).
  - Preserves ANSI color codes for rendering while supporting plain-text filtering.

### Requirement R2: Ephemeral Virtual Localhost Reverse Proxy
- **Proxy Engine (`src/proxy/`)**:
  - Built with Hyper 1.x, Tokio, and HTTP Body Util.
  - Dynamically routes requests based on the HTTP `Host` header (e.g. `api.localhost:8080` $\to$ dynamic upstream port).
  - Transparently streams request and response bodies, chunked transfers, and Server-Sent Events (SSE).
  - Handles bidirectional TCP connection upgrades for WebSockets (RFC 6455).
  - Generates clear diagnostic HTTP 502 (Bad Gateway) and HTTP 503 (Service Starting) pages with tail log snippets.

### Requirement R3: Split-Screen Terminal User Interface (TUI)
- **TUI Engine (`src/tui/`)**:
  - Split-screen layout: Header (status & proxy overview), Left pane (service list with live status glyphs), Right pane (streamed logs), Footer (interactive shortcuts).
  - High-craft electric cyan (`#00E5FF`) and neon violet (`#BD00FF`) aesthetic.
  - Sub-15ms event-driven dirty-flag rendering loop preventing terminal flicker.
  - Interactive hotkeys: `[r]` restart service, `[s]` stop service, `[a]` start all, `[x]` stop all, `[Space]` auto-scroll toggle, `[/]` regex search modal, `[c]` clear buffer, `[p]` proxy dialog, `[q]` quit.
  - RAII panic hook and terminal guard ensuring clean restoration of terminal raw mode.

### Requirement R4: Automated Testing & CI/CD Packaging
- **Comprehensive Test Harness (`tests/`)**:
  - 9 automated test targets with 82 unit, integration, and adversarial tests (100% pass rate).
  - Pure-Rust in-process mock HTTP echo server (`tests/common/mock_http.rs`) supporting HTTP/1.1 and RFC 6455 WebSockets.
  - Synthetic test fixtures for Node.js (`package.json`), Python/Foreman (`Procfile`), Docker Compose (`docker-compose.yml`), and malformed configurations.
  - Windows Job Object grandchild orphan termination verification (`tests/supervisor_tests.rs`).
  - Static AST zero TODO/FIXME verification guard (`tests/no_todos_test.rs`).
- **CI/CD Workflows (`.github/workflows/`)**:
  - `ci.yml`: Cross-platform matrix test on Ubuntu, macOS, and Windows with `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, formatting check (`cargo fmt --check`), clippy linter (`-D warnings`), and test execution.
  - `release.yml`: Automated multi-architecture release compilation across 4 targets (`x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`), archive packaging (.zip / .tar.gz), SHA-256 checksum generation, and GitHub Release publication.

### Requirement R5: Git Initialization & GitHub Launch
- Git repository initialized at `d:\spinmux`, staged, and committed with `feat: initial release of spinmux v0.1.0`.
- Release tagged `v0.1.0`.
- Production release binary compiled via `cargo build --release`.
- Public remote repository created: `https://github.com/shenald-dev/spinmux`.
- Published GitHub Release `v0.1.0` with release binary and release notes.

---

## 3. Test Execution & Verification Matrix

| Test Suite | File | Tests | Passed | Failed | Status |
|---|---|:---:|:---:|:---:|:---:|
| `spinmux` (Library Unit Tests) | `src/lib.rs` | 6 | 6 | 0 | **PASS** |
| `discovery_tests` | `tests/discovery_tests.rs` | 17 | 17 | 0 | **PASS** |
| `supervisor_tests` | `tests/supervisor_tests.rs` | 6 | 6 | 0 | **PASS** |
| `proxy_tests` | `tests/proxy_tests.rs` | 9 | 9 | 0 | **PASS** |
| `ringbuf_tests` | `tests/ringbuf_tests.rs` | 9 | 9 | 0 | **PASS** |
| `cli_tests` | `tests/cli_tests.rs` | 6 | 6 | 0 | **PASS** |
| `tui_tests` | `tests/tui_tests.rs` | 13 | 13 | 0 | **PASS** |
| `adversarial_tests` | `tests/adversarial_tests.rs` | 15 | 15 | 0 | **PASS** |
| `no_todos_test` | `tests/no_todos_test.rs` | 1 | 1 | 0 | **PASS** |
| **TOTAL** | **9 Suites** | **82** | **82** | **0** | **100% PASS** |

### Quality Audit Results
- `cargo check --all-targets`: **Passed (0 errors)**
- `cargo clippy --all-targets -- -D warnings`: **Passed (0 warnings)**
- `cargo fmt --check`: **Passed (0 formatting diffs)**
- `test_zero_todo_or_fixme_in_critical_paths`: **Passed (0 TODO/FIXME comments in `src/`)**
- `test_windows_job_object_grandchild_orphan_cleanup`: **Passed (Zero orphan processes verified)**

---

## 4. GitHub Repository & Release Details

- **Remote URL**: `https://github.com/shenald-dev/spinmux`
- **Release Tag**: `v0.1.0`
- **Release URL**: `https://github.com/shenald-dev/spinmux/releases/tag/v0.1.0`
- **Release Binary Name**: `spinmux.exe`
- **Release Asset Archive**: `spinmux-v0.1.0-x86_64-pc-windows-msvc.zip`
- **Git Commit Message**: `feat: initial release of spinmux v0.1.0`

---

## 5. Operational & User Walkthrough Guide

### Running `spinmux` on a Project
1. Open a terminal in any directory containing a `package.json`, `Procfile`, or `docker-compose.yml`:
   ```powershell
   cd d:\my-web-project
   spinmux
   ```
2. `spinmux` will auto-discover the services, allocate dynamic ports, launch them encapsulated in Windows Job Objects, start the reverse proxy on port 8080, and render the split-screen TUI.
3. Open your browser and navigate to:
   - `http://frontend.localhost:8080`
   - `http://api.localhost:8080`
4. Use `[Tab]` to navigate between services, `[r]` to restart a service, `[Space]` to lock/unlock log scrolling, or `[/]` to search logs.
5. Press `[q]` to shut down. All child processes and background tasks are immediately and cleanly terminated.

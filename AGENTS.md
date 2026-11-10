# AGENTS.md

Guidance for AI agents (Devin, Claude, Cursor, etc.) working in this repository.

The **README is the source of truth** for layout, port phases, commands, and
conventions — read it first. This file adds the sub-agent delegation policy.

## Delegating to sub-agents

The remaining port work (README "Port phases" 4–6) is deliberately separable.
When a task maps cleanly to one workstream below, delegate it to a sub-agent
instead of doing everything serially. Two engines are available — pick per
task (Devin for implementation, OpenCode for a second opinion or a cheaper
model):

- `run_subagent` with profile **`devin-delegate`** — shells out to `devin -p`
- `run_subagent` with profile **`opencode-delegate`** — shells out to
  `opencode run` (`--agent`/`--model` pass through the brief)

(Both are thin wrappers over `~/.local/bin/agent-delegate`, same as the
`/delegate` skill.) Workstreams are ordered by dependency — rows in the same
tier can run in parallel.

### Workstreams & write scopes

| Tier | Workstream | Owns (write scope) | Ports from (`/Users/rithyskun/Developments/openshares`) |
|---|---|---|---|
| 1 | crypto | `crates/core/src/crypto/**` | `src/lib/{auth,jwt,envelope,signing,security}.ts` |
| 1 | data layer | `crates/core/src/models.rs`, `crates/core/src/db/**` | `migrations/` + the SQL inside `src/app/api/**/route.ts` |
| 2 | media serving | `crates/server/src/media*` (`serve_media`, Range, tee-to-cache) | `src/lib/storage.ts`, `src/app/api/media/by-token/` |
| 3 | public routes | `crates/server/src/routes/public*` (`by-token`, `/api/public/*`, `/api/bot/webhook`) | `src/app/api/{public,media/by-token,bot/webhook}/` |
| 3 | middleware | `crates/server/src/middleware/**` (order: correlation-id → security headers → same-origin → rate-limit → CSRF) | `src/middleware.ts` |
| 4 | authenticated routes | `crates/server/src/routes/**` minus `public*` | `src/app/api/**` (~25 routes) |

### Rules

- **One sub-agent per workstream.** Never run two sub-agents whose write
  scopes overlap. `Cargo.toml` is shared — either pre-add the deps yourself
  before launching, or have each agent work behind a `#[cfg]` stub and wire
  deps centrally.
- **Respect the tiers.** A sub-agent may only assume lower-tier workstreams
  exist (crypto + data layer first; routes last). If it needs an interface
  from a same-tier workstream, define the signature yourself in the brief.
- **Write self-contained briefs.** Include: the goal, TS source path(s),
  target Rust file(s), repo conventions (module doc comment naming the TS
  counterpart; `tracing` for logging; `unwrap`/`expect` lint-denied outside
  tests; CPU-bound crypto via `tokio::task::spawn_blocking`; streams via
  `Body::from_stream`, never buffered), and the done-criteria + quality
  requirements below.
- **Keep contracts stable.** If a sub-agent needs a new shared type or a
  public API from `core`, state the exact signature in its brief — don't let
  two agents invent the same thing differently.

### Done-criteria (every delegated task)

Rust is installed natively via `brew install rustup` (PATH:
`/opt/homebrew/opt/rustup/bin` — prepend it if `cargo` isn't found). Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Docker (README) is only a fallback — keep its target volume separate and
never share `./target` between macOS and Linux runs.

Green gates are necessary but not sufficient — code without the tests,
review, and security pass below is not done.

### Tests, review & security (every delegated task)

- **Unit tests ship with the port, not after it.** Port the TS behavior's
  test expectations where the source repo has them; otherwise write tests
  that pin the observable behavior (edge cases, error paths, byte-for-byte
  formats the TS side depends on — see `util::tests` for the style). Tests
  live in `#[cfg(test)] mod tests` at the bottom of the ported file;
  repo-level DB tests go in `db/repos/tests.rs`. `unwrap`/`expect` are
  allowed in tests only.
- **Self-review before reporting done.** Re-read the complete diff against
  the TS source: dropped branches/edge cases, off-by-one in Range or
  pagination math, error paths that swallow failures into `200`s, status
  codes and header names that drifted from the TS responses.
- **Vulnerability check.** This codebase is auth + crypto + untrusted
  media — actively look for and fix: panic paths on attacker-controlled
  input; secret/token comparisons that aren't constant-time (use `subtle`,
  the `secretEq` counterpart); path traversal in media/cache file access;
  non-parameterized SQL (`Db::query_*`/`execute` with `libsql::params!`
  binds only — never string-built queries; raw `conn().query` is off-limits
  outside `db/mod.rs` — use `query_guard`, which carries the remote
  timeout + stale-conn retry every Turso call needs);
  unbounded request-body or stream buffering; missing ownership/authz
  checks on authenticated routes. If the TS source has the same bug,
  still fix it — and say so in the report.
- **Dependency vulns.** Run `cargo audit` on the lockfile when deps were
  added or changed (one-time install: `cargo install cargo-audit --locked`).
  Any advisory on a newly added dep must be resolved or flagged.
  Known-accepted residuals (libsql 0.10's own `hyper 0.14`/`rustls 0.22`
  remote stack — not independently upgradable; revisit when libsql revs):
  RUSTSEC-2026-0258 (h2), -2026-0346/-0349 (rustls),
  -2026-0104/-0098/-0099/-0049/-0382 (rustls-webpki), -2025-0134
  (rustls-pemfile, unmaintained).

### Verify yourself

Sub-agent output is untrusted until checked: run the gates above and review
the diff yourself — correctness against the TS source plus the vuln
checklist — before treating delegated work as done. Never touch files
inside a running sub-agent's write scope.

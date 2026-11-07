# Codex fork (jingyugao/codex)

A thin fork of [openai/codex](https://github.com/openai/codex). It changes only what has to change
inside Codex and has no upstream extension point, and it exposes registration hooks for host
binaries. High availability (Postgres storage, cross-pod handoff, sandbox binding) and multi-user
chat live in [jingyugao/hcodex](https://github.com/jingyugao/hcodex), which builds on this fork.
The upstream README is [here](../README.md).

## What the fork adds

### Per-turn environment variables (`fork.env.*`)

Clients pass `fork.env.<NAME>` entries in the existing `turn/start` field
`responsesapiClientMetadata`. With the prefix stripped, they become environment variables for the
turn's `exec_command` calls, local command hooks and remote environment hooks:

```json
{"method":"turn/start","params":{"threadId":"…","input":[…],
  "responsesapiClientMetadata":{"fork.env.TRACE_ID":"abc123"}}}
```

- These entries are removed from metadata sent to model providers and MCP servers.
- Upstream keeps at most 16 metadata entries, with keys up to 64 bytes and values up to 128 bytes.
  The fork also drops entries whose name does not match `[A-Za-z_][A-Za-z0-9_]*` or whose value
  contains NUL. Both are **dropped silently**; clients must validate.
- `thread/queue/add` takes the same field (upstream already supports it); the metadata becomes the
  turn's metadata when the item is dispatched.
- A turn recovered on another pod has no client metadata; hosts can carry the env in the turn
  extension data `codex_core::fork_hooks::ForkTurnEnv`.
- Clients must enable `experimentalApi`.

### Resume with a non-local thread store

`thread/resume` expected a rollout file path, which non-local stores (such as hcodex's Postgres
store) do not have. The fork treats a missing path as empty and continues. `thread/revert` still
does not work with non-local stores.

### Registration hooks

Host binaries call these before starting the app-server. Without them the behavior matches
upstream, except for per-turn env. The fork depends on no host crate.

| Function                                                   | Effect                                                                          |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `codex_core::fork_hooks::set_thread_store_factory(f)`      | When `f(&Config)` returns `Some(store)`, it replaces the thread store; set once |
| `codex_app_server::fork_hooks::set_queue_store_factory(f)` | The same for the queued-input store                                             |
| `codex_app_server::fork_hooks::add_extension_installer(f)` | Installs extensions whenever the thread extension registry is built             |

## How it stays mergeable

- Fork code lives in each crate's `fork_hooks/` directory. Upstream files only gain one-line
  `crate::fork_hooks::…(…)` calls marked `// FORK`; `git grep -n fork_hooks -- codex-rs ':!*/fork_hooks/*'`
  lists every one.
- Code that needs private upstream fields is attached as a child module at the end of that file:
  `#[path = "fork_hooks/xxx.rs"] mod fork;`. Never put `..` in the `#[path]`; `cargo shear` does not
  resolve it.
- No changes to `Cargo.toml` / `Cargo.lock`, config types, protocol definitions or generated files.
- All changes to existing upstream files:
  `git diff <upstream tag>... --stat -- codex-rs ':!codex-rs/*/fork_hooks/*'` (currently 11 files,
  +33/−1).

The fragile spots are `core/src/unified_exec/process_manager.rs` and
`core/src/hook_environment_executor.rs`: upstream changes them often, and a change can compile while
the env no longer arrives. hcodex's end-to-end tests check that it reaches commands and hooks; run
them after every upstream sync.

## Syncing upstream

Follow upstream release tags (`rust-v*`) with `git merge <tag>`; do not rebase or force-push.
`git config rerere.enabled true` helps with repeated conflicts. After merging, run the checks
below, then bump `codex.rev` in hcodex and run its tests.

## Local checks

Day-to-day checks run locally. CI (`.github/workflows/fork-release.yml`) only verifies `fork-v*`
tags after the fact. Upstream's workflows are disabled in this repository's Actions settings, so
their format and dependency checks run here too:

```sh
cd codex-rs
cargo test -p codex-core --lib fork_hooks
cargo test -p codex-hooks --lib
cargo clippy -p codex-core -p codex-hooks -p codex-app-server --lib -- -D warnings
git diff --name-only --diff-filter=d main -- '*.rs' | sed 's|^codex-rs/||' | xargs rustfmt --edition 2024 \
  --config-path rustfmt.toml --config skip_children=true --check
cargo shear --deny-warnings        # cargo +1.95.0 install cargo-shear --version 1.11.2 --locked
npx prettier@3.5.3 --check ../.github/workflows/fork-release.yml
```

Size build parallelism by memory: each compile job needs about 2–3 GB.

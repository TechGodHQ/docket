# docket

A **decision ledger** for the TechGodHQ fleet. Docket holds items that need
a ruling and nothing else: it is a ledger, never an executor. Every item
arrives decision-ready — injected context, a stated recommendation, and a
declarative action descriptor naming an existing worker that will carry out
the decision (e.g. `hermes-coding` for merges, `comms` for email).

**North-star metric:** throughput per minute of Shiv's attention. Items that
arrive half-formed cost attention, so half-formed items are producer bugs
and are rejected at the boundary.

## Design laws

1. **Ledger, never executor.** Core performs no actions and contains no
   LLM. `action_descriptor`s are declarative; dispatch layers hand them to
   existing workers.
2. **Decision-ready or rejected.** Every item must ship with context and a
   recommendation plus an action descriptor. Missing any of these is a
   producer bug (`validate_item` rejects it).
3. **Core/UI strictly separated.** The HTTP surface is a clean API
   (`api/operations.yaml` → hydra projections: CLI/HTTP/MCP/TS client) that
   a separate UI repo consumes. The initial UI is the Alice Board; the
   long-term product is a real UI with self-host / paid-hosting model.
4. **Append-only decisions.** `decision_log` records who decided what and
   when; history is never rewritten.

## Layout

| Crate | Purpose |
|-------|---------|
| `crates/docket-core` | Domain model: `Item`, `ActionDescriptor`, `DecisionLogEntry`, producer-boundary validation, store trait (+ in-memory store; SQLite under `/data` lands with the storage ticket) |
| `crates/docket-cli` | Hydra-projected operations: CLI, HTTP (axum), MCP manifest, TS client |
| `crates/docket-codegen` | Codegen shim over the pinned `hydra-codegen` |

Hydra is the sole projection mechanism, pinned to the immutable
[`v0.3.0` tag](https://github.com/TechGodHQ/hydra) of TechGodHQ/hydra.
`api/operations.yaml` is the single source of truth; generated artifacts are
committed and CI fails if they go stale.

## Development

```sh
cargo build --locked --all-targets
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
# regenerate committed hydra artifacts (run from crates/docket-cli):
cd crates/docket-cli && cargo run -p docket-codegen --bin docket-codegen -- write
```

## HTTP surface

```sh
cargo run -p docket-cli --bin docket-http   # listens on 127.0.0.1:8951
curl -s localhost:8951/docket
curl -s "localhost:8951/items?status=pending"
```

`POST /items` enforces design law 2: no context, recommendation, or action
descriptor → the create is rejected as a producer bug.

## License

MIT — see [LICENSE](LICENSE).

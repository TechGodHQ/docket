# Docket — Context & Design Laws

Docket is the TechGodHQ **decision queue**: a ledger of items awaiting a
ruling, plus the declarative instruction for what happens after the ruling.
Its north-star metric, set by Shiv (2026-11-17): **throughput per minute of
Shiv's attention.** Everything below serves that number — items must arrive
decision-ready, or they cost attention instead of saving it.

## Law 1 — Docket is a ledger, never an executor

Docket records decisions; it never performs them. There is **no LLM in
core** and no side-effecting action anywhere in `docket-core`. Outcomes are
expressed as declarative `action_descriptor`s dispatched to *existing*
workers:

- merges → `hermes-coding`
- email → `comms`
- (future workers are added by name, never by embedding execution here)

If a feature seems to require docket to *do* something, the feature belongs
in a worker, not in docket.

## Law 2 — Every item arrives decision-ready

An item MUST ship with:

1. **Injected context** (markdown) — everything the decider needs, gathered
   by the producer.
2. **A recommended action** — stated plainly.
3. **An `action_descriptor`** — target worker + operation + params.

An item missing any of the three is a **producer bug**, not user input.
`docket_core::validate_item` enforces this at the boundary and the store
refuses the write. Producers (Plane adapter, Telegram digest, inbox
watchers) must never file undecidable items; they refine first or don't
file.

## Law 3 — Core and UI are strictly separated, from day one

- The HTTP surface (`crates/docket-cli/api/operations.yaml`, projected by
  Hydra v0.3.0 into CLI/HTTP/MCP/TS-client) is a **clean API**. It is the
  contract; a separate UI repo consumes it.
- The initial UI is the **Alice Board** (Atlas dashboard) reading this API.
- Long term: a real UI with a self-host / paid-hosting product model. Any
  UI-flavored convenience added to core or the API now is debt against that
  product. No HTML, no sessions, no rendering in docket.

## Law 4 — The decision log is append-only

`decision_log` entries record `at`, `actor`, `decision`, `note`. Entries are
never edited or deleted; corrections are new entries. The ledger's value is
that it is a trustworthy record of what was decided and why.

## Item model

`id` (`dkt-…`), `source`, `title`, `context` (markdown), `recommendation`,
`action_descriptor` (`{target, operation, params}`), `status`
(`pending|dispatched|dismissed`), `decision_log[]`.

Storage: SQLite under the `/data` volume (deployment); the store trait is
the seam.

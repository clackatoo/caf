CAF, the Clackatoo Agent Framework, is a Rust library for building robust,
feature-rich embedded chats with agents over ACP. It grows out of
clackatoo's agent backend (~/projects/clackatoo). The API is most of the
product and robustness the rest.

You MUST NOT use your standard harness-provided memory system. CAF keeps
its knowledge in Acta, workspace `caf`, task prefix CAF:
- Active work → tasks.
- Decisions about a specific piece of work, with their reasoning,
  alternatives and trade-offs → that task's description or comments.
- Cross-cutting, standalone knowledge → Acta Memories at workspace scope.

Before starting real work, run `memory_recall(workspace: "caf")` and read
`design-approach` and `architecture-decisions`. Features are designed
whole, their API reviewed by jack, before they're built.

Keeping Acta and the docs up to date is part of doing the work, not an
afterthought. The repo is public, so anything on main is public anyway.

The TypeScript caf-sdk's types (`ts/sdk/src/types/`), and real JSON
typed with them (`ts/sdk/test/shapes.gen.ts`), are generated from
caf-core and checked in. After changing a type CAF serialises,
regenerate them with
`CAF_BLESS=1 cargo test -p caf-core --features ts typescript_types`;
CI fails while they're stale.

The conformance fixtures in `fixtures/apply/` (each chat in the core's
tests, change by change, which every SDK's `apply` must follow) are
written by the core's test harness. After changing what the core
records, regenerate them with `CAF_BLESS=1 cargo test -p caf-core`; the
tests fail while any is stale, missing or extra.

`samples/viewer/v1.json` holds one of every message of version 1 of the
viewer wire, and a test fails if what CAF writes no longer matches it.
What a released version sends never changes: a change to it needs a new
version with its own sample.

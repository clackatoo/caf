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

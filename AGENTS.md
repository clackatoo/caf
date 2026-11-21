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
tests, change by change, and each change `apply` must refuse, which
every SDK must follow) are written by caf-core's tests. After changing
what the core records, regenerate them all with
`rm -rf fixtures/apply && CAF_BLESS=1 cargo test -p caf-core`. The tests
fail while one is stale or missing, or names a test that's gone; CI
also regenerates them and fails if anything in the folder changed, which
is what finds a fixture no test writes any more.

`samples/viewer/v1.json` holds one of every message of version 1 of the
viewer wire, and of every variant inside them. A test fails if it no
longer reads and writes back as the same bytes. What a released version
sends never changes: a change to it needs a new version with its own
sample. `CAF_BLESS` never touches it; only while a version is
unreleased may `CAF_BLESS_SAMPLE=1 cargo test -p caf-core version_1`
rewrite its sample.

`samples/save/v1/` holds a chat saved by version 1 of the saved format,
with one of every kind of item, turn, change and journal entry, an app
item and data slot included: the format's own bytes (`header`,
`snapshot`, `journal`), and the same chat as caf-save's `FileStore`
keeps it (`files/`, with a second chat, squeezed). A test fails if it no
longer reads as the values in `crates/save/tests/sample.rs` say: it's
pinned by what it reads as, not its bytes, as zstd may compress the
same thing differently from one version to the next. Saves stay
readable forever, and `caf_save::VERSION` goes up with every change to
what's saved, a field or variant added included, so an older CAF never
reads a newer save and writes it back short. `shapes.ts` pins the saved
types' shapes, from the TypeScript types: a test fails if they, or what
CAF writes of the sample's values, change while `VERSION` doesn't. A new
version gets a sample of its own. Only while a version is unreleased may
`CAF_BLESS_SAMPLE=1 cargo test -p caf-save version_1` rewrite its sample.

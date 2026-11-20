// The types of everything CAF sends and takes, generated from caf-core's
// own by ts-rs into `types/` (see caf-core's `ts.rs`), and checked in: CI
// fails if they're stale. Regenerate them from the repository's root with
// `CAF_BLESS=1 cargo test -p caf-core --features ts typescript_types`.
//
// Every CAF enum is `{ type, value }`, so `switch (x.type)` narrows it.
// The generic ones (`Chat`, `Turn`, `Item`, `ItemKind`, `ToolCall`,
// `Change`, `Numbered`, `ToViewer`, `Event`, `HostCommand`) are generated
// with no app items of their own: `NoAppItems`, which is `never`.

export type * from "./types/index.ts";

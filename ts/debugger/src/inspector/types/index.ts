// The inspector channel's types, generated from caf-debugger's `inspector.rs`:
// don't edit. The channel is unstable: it changes with the app, and its
// version is `PROTOCOL`, in `Hello`. Regenerate them from the repository's
// root with
// `CAF_BLESS=1 cargo test -p caf-debugger --features ts inspector_types`.
export type * from "./ToInspector.ts";
export type * from "./Mode.ts";
export type * from "./Check.ts";
export type * from "./Step.ts";
export type * from "./FromInspector.ts";

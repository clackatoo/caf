// The inspector channel's types, generated from caf-debugger's
// `inspector.rs` into `./types/`. The channel is unstable: only this app
// reads it. See that module's docs for the order things come in.

export type * from './types/index.ts'

/**
 * The channel's version this app is built for: caf-debugger's `PROTOCOL`
 * (`inspector.rs`), which ts-rs can't export. A test checks they match.
 */
export const PROTOCOL = 1

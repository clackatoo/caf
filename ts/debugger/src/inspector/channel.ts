// The inspector channel's types, as caf-debugger's `inspector.rs` defines
// them (branch f2-server, 3d92a6d). Its generated types land in
// `./types/` when that branch merges; this file then becomes
// `export type * from './types/index.ts'`.
//
// The channel is unstable: only this app reads it. See the module docs of
// `crates/debugger/src/inspector.rs` for the order things come in.

import type { Directive, Event, JsonValue, Now, ToViewer } from '@clackatoo/caf'

export type Mode = { type: 'Live' } | { type: 'Replay' }

/** One step: an event the core was given, and what it gave out. Numbered from 1. */
export type Step = { no: number; now: Now; event: Event; directives: Array<Directive> }

export type ToInspector =
  | { type: 'Hello'; value: { mode: Mode; file: string | null; run: string | null } }
  | { type: 'Steps'; value: { from: number; steps: Array<Step> } }
  | { type: 'Position'; value: { live: boolean; at: number; end: number } }
  | { type: 'State'; value: { at: number; core: JsonValue } }
  | { type: 'Past'; value: { at: number; message: ToViewer } }
  | { type: 'Diverged'; value: { at: number; expected: Array<Directive>; got: Array<Directive> | null } }

export type FromInspector = { type: 'Seek'; value: { step: number } } | { type: 'Follow' } | { type: 'StateAt'; value: { step: number } }

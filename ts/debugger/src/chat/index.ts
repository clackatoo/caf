// CAF's reference chat view: what a host shows, and can copy.
//
// It imports only `@clackatoo/caf`, Svelte, markdown-it and its own files,
// so this folder can be lifted out whole. `scripts/check-imports.mjs`
// checks that in CI.

export { default as ChatView } from './ChatView.svelte'
export { default as ChatList } from './ChatList.svelte'
export type { ChatActions, ListActions } from './actions.ts'

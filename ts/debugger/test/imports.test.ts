// The import check passes on the chat view as it is, and fails on a copy
// of it with one breach added: importing the inspector or another of the
// app's files however it's spelt, a package it may not use, CSS that
// reaches outside, or {@html} outside Markdown.svelte however it's spelt.

import { execFileSync } from 'node:child_process'
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { afterAll, describe, expect, it } from 'vitest'

const app = resolve(import.meta.dirname, '..')
const script = join(app, 'scripts', 'check-imports.mjs')
// Inside the app, so the copies find its node_modules.
const scratch = join(app, '.check-copies')

function check(root: string): { ok: boolean; out: string } {
  try {
    return { ok: true, out: execFileSync('node', [script, root], { encoding: 'utf8', stdio: 'pipe' }) }
  } catch (error) {
    const failed = error as { stderr?: string; stdout?: string }
    return { ok: false, out: `${failed.stdout ?? ''}${failed.stderr ?? ''}` }
  }
}

afterAll(() => rmSync(scratch, { recursive: true, force: true }))

/** A copy of the app's src/chat, and the rest of src beside it, with files added to src/chat. */
function withFiles(added: Record<string, string>): string {
  mkdirSync(scratch, { recursive: true })
  const root = mkdtempSync(join(scratch, 'copy-'))
  cpSync(join(app, 'src'), join(root, 'src'), { recursive: true })
  for (const [name, source] of Object.entries(added)) writeFileSync(join(root, 'src', 'chat', name), source)
  return root
}

// Each case runs the whole check, a Vite build of the chat view: on a busy
// machine that takes far longer than vitest's default 5 s.
describe('the import check', { timeout: 60_000 }, () => {
  it('passes the chat view as it is', () => {
    const result = check(app)
    expect(result.out).toContain('stands alone')
    expect(result.ok).toBe(true)
  })

  it('passes a copy with nothing added', () => {
    expect(check(withFiles({})).ok).toBe(true)
  })

  const breaches: [string, Record<string, string>, string][] = [
    ['a static import of the inspector', { 'Bad.svelte': `<script lang="ts">import { Inspector } from '../inspector/client.svelte.ts'; void Inspector</script>` }, 'outside src/chat/'],
    ["one of the app's files", { 'bad.ts': `export { socketUrl } from '../link.ts'` }, 'outside src/chat/'],
    ['a dynamic import', { 'bad.ts': `export const lazy = () => import('./words.ts')` }, 'at run time'],
    ['a template-literal import', { 'bad.ts': 'export const lazy = () => import(`../link.ts`)' }, 'src/chat/bad.ts'],
    ['import.meta.glob', { 'bad.ts': `export const all = import.meta.glob('../inspector/*.ts')` }, 'src/chat/bad.ts'],
    ['an import strip() would have hidden between comment-like strings', { 'bad.ts': `const s = '/*'\nimport { socketUrl } from '../link.ts'\nconst t = '*/'\nexport { s, t, socketUrl }` }, 'outside src/chat/'],
    ['an import after a regex with //', { 'bad.ts': `const re = /\\/\\//; import { socketUrl } from '../link.ts'\nexport { re, socketUrl }` }, 'outside src/chat/'],
    ['a .mts file', { 'bad.mts': `export { socketUrl } from '../link.ts'` }, 'outside src/chat/'],
    ['a package path that climbs out', { 'bad.ts': `export * from 'svelte/../../../debugger/src/link.ts'` }, 'src/chat/bad.ts'],
    ['a package it may not use', { 'bad.ts': `export { parse } from 'vite'` }, "isn't @clackatoo/caf"],
    ['CSS @import without quotes', { 'bad.css': `@import url(../../app.css);`, 'bad.ts': `import './bad.css'` }, 'CSS @import'],
    ['CSS url() in a style block', { 'Bad.svelte': `<p>x</p><style>p { background: url(https://example.com/x.png) }</style>` }, 'CSS url()'],
    ['{@html}', { 'Bad.svelte': `<p>{@html '<b>x</b>'}</p>` }, 'uses {@html}'],
    ['{ @html } with spaces', { 'Bad.svelte': `<p>{ @html "<b>x</b>"}</p>` }, 'uses {@html}'],
    ['a type-only import', { 'bad.ts': `import type { Inspector } from '../inspector/client.svelte.ts'\nexport type X = Inspector` }, 'outside src/chat/'],
    ['a type-only export', { 'bad.ts': `export type { LinkState } from '../link.ts'` }, 'outside src/chat/'],
    [
      "a type-only import in a component's script",
      { 'Bad.svelte': `<script lang="ts">import type { Inspector } from '../inspector/client.svelte.ts'; let { i }: { i: Inspector } = $props()</script><p>{String(i)}</p>` },
      'outside src/chat/',
    ],
    ['a type-only import of a package it may not use', { 'bad.ts': `import type { UserConfig } from 'vite'\nexport type X = UserConfig` }, "isn't @clackatoo/caf"],
    ['new URL(…, import.meta.url)', { 'bad.ts': `export const u = new URL('../inspector/client.svelte.ts', import.meta.url).href` }, 'import.meta.url'],
    ['a CSS file nobody imports', { 'stray.css': `@import url(https://evil.example/x.css);` }, 'CSS @import'],
    ['bind:innerHTML', { 'Bad.svelte': `<script>let s = $state('')</script><div contenteditable="true" bind:innerHTML={s}></div>` }, 'innerHTML'],
    ['innerHTML in a script', { 'Bad.svelte': `<script>let el; $effect(() => { el.innerHTML = '<b>x</b>' })</script><div bind:this={el}></div>` }, 'innerHTML'],
    ["innerHTML by its name in brackets", { 'bad.ts': `export function put(el: Element, s: string) { el['innerHTML'] = s }` }, 'innerHTML'],
    ['insertAdjacentHTML', { 'bad.ts': `export function put(el: Element, s: string) { el.insertAdjacentHTML('beforeend', s) }` }, 'insertAdjacentHTML'],
    ['outerHTML in a handler', { 'Bad.svelte': `<button onclick={(e) => (e.currentTarget.outerHTML = 'x')}>x</button>` }, 'outerHTML'],
    ['document.write', { 'bad.ts': `export function put(s: string) { document.write(s) }` }, 'document.write'],
    ['createContextualFragment', { 'bad.ts': `export const f = (s: string) => document.createRange().createContextualFragment(s)` }, 'createContextualFragment'],
    ['srcdoc', { 'Bad.svelte': `<iframe title="x" srcdoc="<b>x</b>"></iframe>` }, 'srcdoc'],
    ['setHTMLUnsafe', { 'bad.ts': `export function put(el: Element, s: string) { (el as any).setHTMLUnsafe(s) }` }, 'setHTMLUnsafe'],
    ['parseHTMLUnsafe', { 'bad.ts': `export const d = (s: string) => (Document as any).parseHTMLUnsafe(s)` }, 'parseHTMLUnsafe'],
    [
      "DOMParser's parseFromString",
      { 'bad.ts': `export function put(el: Element, s: string) { el.append(...new DOMParser().parseFromString(s, 'text/html').body.childNodes) }` },
      'parseFromString',
    ],
    ['window.document.write', { 'bad.ts': `export function put(s: string) { window.document.write(s) }` }, 'document.write'],
    ['globalThis.document.writeln', { 'bad.ts': `export function put(s: string) { globalThis.document.writeln(s) }` }, 'document.write'],
    ['document.write in a component', { 'Bad.svelte': `<button onclick={() => window.document.write('x')}>x</button>` }, 'document.write'],
    ['a triple-slash reference', { 'bad.ts': `/// <reference path="../link.ts" />\nexport const a = 1` }, 'outside src/chat/'],
    ['a .js file', { 'bad.js': `/** @typedef {import('../link.ts').LinkState} L */\nexport const a = 1` }, '.js'],
  ]
  for (const [what, files, says] of breaches) {
    it(`fails ${what}`, () => {
      const result = check(withFiles(files))
      expect(result.ok, result.out).toBe(false)
      expect(result.out).toContain(says)
    })
  }

  it("doesn't count an import in a comment", () => {
    expect(check(withFiles({ 'fine.ts': `// import x from '../inspector/x.ts'\n/* import y from 'left-pad' */\nexport {}` })).ok).toBe(true)
  })
})

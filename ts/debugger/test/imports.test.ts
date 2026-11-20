// The import check passes on the chat view as it is, and fails on a copy
// of it that imports the inspector, another of the app's files, an npm
// package it may not use, or uses {@html} outside Markdown.svelte.

import { execFileSync } from 'node:child_process'
import { cpSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'

const app = resolve(import.meta.dirname, '..')
const script = join(app, 'scripts', 'check-imports.mjs')

function check(root: string): { ok: boolean; out: string } {
  try {
    return { ok: true, out: execFileSync('node', [script, root], { encoding: 'utf8', stdio: 'pipe' }) }
  } catch (error) {
    const failed = error as { stderr?: string; stdout?: string }
    return { ok: false, out: `${failed.stdout ?? ''}${failed.stderr ?? ''}` }
  }
}

let copies: string[] = []
afterEach(() => {
  for (const copy of copies) rmSync(copy, { recursive: true, force: true })
  copies = []
})

/** A copy of the app's src/chat, with one file added. */
function withFile(name: string, source: string): string {
  const root = mkdtempSync(join(tmpdir(), 'caf-imports-'))
  copies.push(root)
  cpSync(join(app, 'src', 'chat'), join(root, 'src', 'chat'), { recursive: true })
  writeFileSync(join(root, 'src', 'chat', name), source)
  return root
}

describe('the import check', () => {
  it('passes the chat view as it is', () => {
    expect(check(app).ok).toBe(true)
  })

  const breaches: [string, string, string][] = [
    ['Bad.svelte', `<script lang="ts">import { Inspector } from '../inspector/client.svelte.ts'</script>`, 'outside src/chat/'],
    ['bad.ts', `import { socketUrl } from '../link.ts'`, 'outside src/chat/'],
    ['bad2.ts', `export { x } from "../../somewhere/else.ts"`, 'outside src/chat/'],
    ['bad3.ts', `const lazy = import('../inspector/Inspector.svelte')`, 'outside src/chat/'],
    ['bad4.ts', `import confetti from 'canvas-confetti'`, "isn't @clackatoo/caf"],
    ['bad5.ts', `import './inspector-styles.css'`, 'from the inspector'],
    ['Bad2.svelte', `<p>{@html '<b>x</b>'}</p>`, 'uses {@html}'],
  ]
  for (const [name, source, says] of breaches) {
    it(`fails ${source}`, () => {
      const result = check(withFile(name, source))
      expect(result.ok).toBe(false)
      expect(result.out).toContain(says)
    })
  }

  it("doesn't count an import in a comment", () => {
    expect(check(withFile('fine.ts', `// import x from '../inspector/x.ts'\n/* import y from 'left-pad' */\nexport {}`)).ok).toBe(true)
  })
})

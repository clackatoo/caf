// Checks the chat view can be lifted out whole: every file in src/chat/
// imports only @clackatoo/caf, Svelte, markdown-it and files of its own,
// never the inspector or anything else of the app's. And only one file in
// it, Markdown.svelte, puts HTML in the page as it is.
//
// Run in CI: `npm run check:imports`. It exits non-zero on any breach.

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const chat = join(root, 'src', 'chat')

/** Packages the chat view may use: the SDK, Svelte, and the Markdown renderer. */
const packages = [/^@clackatoo\/caf$/, /^svelte(\/.*)?$/, /^markdown-it$/]
/** The one file allowed `{@html}`: it renders Markdown with HTML off. */
const html = new Set([join(chat, 'Markdown.svelte')])

function files(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return files(path)
    return /\.(ts|js|mjs|svelte|css)$/.test(name) ? [path] : []
  })
}

// `import … from 'x'`, `import 'x'`, `export … from 'x'`, `import('x')`,
// and CSS `@import 'x'`. Comments are stripped first, so an import in
// prose isn't counted.
const specifiers = /(?:\bimport\s*(?:[^'"`;]*?\bfrom\s*)?|\bexport\s+[^'"`;]*?\bfrom\s*|\bimport\s*\(\s*|@import\s+(?:url\()?)['"]([^'"]+)['"]/g

function strip(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/<!--[\s\S]*?-->/g, '').replace(/(^|[^:'"])\/\/.*$/gm, '$1')
}

const breaches = []
for (const file of files(chat)) {
  const source = readFileSync(file, 'utf8')
  const name = relative(root, file)
  for (const [, spec] of strip(source).matchAll(specifiers)) {
    if (spec.startsWith('.')) {
      const target = resolve(dirname(file), spec)
      if (target !== chat && !target.startsWith(chat + sep)) {
        breaches.push(`${name}: imports ${spec}, outside src/chat/`)
      }
    } else if (!packages.some((allowed) => allowed.test(spec))) {
      breaches.push(`${name}: imports ${spec}, which isn't @clackatoo/caf, Svelte or markdown-it`)
    }
    if (/inspector/.test(spec)) breaches.push(`${name}: imports ${spec}, from the inspector`)
  }
  if (/\{@html\b/.test(source) && !html.has(file)) {
    breaches.push(`${name}: uses {@html}, which only Markdown.svelte may`)
  }
}

if (breaches.length > 0) {
  console.error('The chat view must stand alone (see src/chat/index.ts):')
  for (const breach of breaches) console.error(`  ${breach}`)
  process.exit(1)
}
console.log(`src/chat/ stands alone: ${files(chat).length} files checked.`)

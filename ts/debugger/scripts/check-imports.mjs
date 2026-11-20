// Checks the chat view can be lifted out whole, and stays safe:
//
// - **Imports:** every file in src/chat/ is built with Vite, as the app
//   builds it, and each import a file there makes, once resolved, must be
//   a file in src/chat/ or in one of the packages it may use:
//   @clackatoo/caf, Svelte and markdown-it. So the check follows what the
//   bundler does (template literals, `import.meta.glob`, odd paths such as
//   `svelte/../../x`), not the text of the imports. It never imports
//   anything dynamically, so any `import()` of its own fails too.
// - **CSS** in src/chat/ (files and `<style>`) may not `@import` or
//   `url()` anything: Vite resolves those itself, out of a plugin's sight.
// - **`{@html}`**, the one way to put HTML in the page unescaped, appears
//   only in Markdown.svelte, which renders Markdown with HTML off. Each
//   component is parsed with Svelte's own parser, so any spelling it
//   accepts is found.
//
// Run in CI: `npm run check:imports`. It exits non-zero on any breach. It
// takes another folder to check instead of this app's, which its tests use.

import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { parse } from 'svelte/compiler'
import { build } from 'vite'

const root = realpathSync(resolve(process.argv[2] ?? join(dirname(fileURLToPath(import.meta.url)), '..')))
const chat = join(root, 'src', 'chat')

/** Packages the chat view may use. */
const packages = ['@clackatoo/caf', 'svelte', 'markdown-it']
/** The one file allowed `{@html}`. */
const html = join(chat, 'Markdown.svelte')

const breaches = []
const name = (path) => relative(root, path)
const inside = (dir, path) => path === dir || path.startsWith(dir + sep)
const bare = (id) => id.split('?')[0]

/** Where a package is installed, followed through any link, as node finds it from the app. */
function packageDir(pkg) {
  for (let dir = root; ; dir = dirname(dir)) {
    const candidate = join(dir, 'node_modules', pkg)
    if (existsSync(candidate)) return realpathSync(candidate)
    if (dirname(dir) === dir) throw new Error(`${pkg} isn't installed`)
  }
}
const allowed = new Map(packages.map((pkg) => [pkg, packageDir(pkg)]))

/** The package a bare specifier names, such as `svelte` for `svelte/internal/client`. */
function packageOf(specifier) {
  const parts = specifier.split('/')
  return specifier.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0]
}

function sources(dir) {
  return readdirSync(dir).flatMap((entry) => {
    const path = join(dir, entry)
    if (statSync(path).isDirectory()) return sources(path)
    return [path]
  })
}

const files = sources(chat)
const code = files.filter((path) => /\.(svelte|[cm]?[jt]sx?)$/.test(path))

/** Fails an import from src/chat/ that resolves anywhere it mayn't. */
const guard = {
  name: 'caf-chat-guard',
  enforce: 'pre',
  async resolveId(source, importer, options) {
    if (importer === undefined || !inside(chat, bare(importer)) || source.startsWith('\0')) return null
    const from = name(bare(importer))
    let resolved
    try {
      resolved = await this.resolve(source, importer, { ...options, skipSelf: true })
    } catch {
      // Such as a path into a package that its exports don't allow.
      breaches.push(`${from}: imports ${source}, which doesn't resolve to a file`)
      return { id: source, external: true }
    }
    const where = resolved === null ? null : bare(resolved.id)
    if (where === null || resolved.external) {
      breaches.push(`${from}: imports ${source}, which doesn't resolve to a file`)
    } else if (source.startsWith('.') || source.startsWith('/')) {
      if (!inside(chat, where)) breaches.push(`${from}: imports ${source}, outside src/chat/`)
    } else {
      const pkg = packageOf(source)
      const dir = allowed.get(pkg)
      if (dir === undefined) breaches.push(`${from}: imports ${source}, which isn't @clackatoo/caf, Svelte or markdown-it`)
      else if (!inside(dir, where)) breaches.push(`${from}: imports ${source}, which resolves outside ${pkg}`)
    }
    return resolved
  },
  resolveDynamicImport(specifier, importer) {
    if (importer !== undefined && inside(chat, bare(importer))) {
      breaches.push(`${name(bare(importer))}: imports something at run time, ${typeof specifier === 'string' ? specifier : 'by an expression'}`)
    }
    return null
  },
  transform(source, id) {
    const path = bare(id)
    const css = /\.css$/.test(path) || /[?&]lang\.css\b/.test(id)
    if (!css || !inside(chat, path)) return null
    const plain = source.replace(/\/\*[\s\S]*?\*\//g, '')
    if (/@import\b/i.test(plain)) breaches.push(`${name(path)}: CSS @import, which Vite resolves out of this check's sight`)
    if (/\burl\s*\(/i.test(plain)) breaches.push(`${name(path)}: CSS url(), which may load anything`)
    return null
  },
}

try {
  await build({
    root,
    configFile: false,
    logLevel: 'silent',
    plugins: [svelte({ configFile: false }), guard],
    build: {
      write: false,
      emptyOutDir: false,
      lib: { entry: code, formats: ['es'] },
    },
  })
} catch (error) {
  breaches.push(`src/chat/ doesn't build: ${error instanceof Error ? error.message.trim().split('\n').slice(0, 6).join(' ') : String(error)}`)
}

/** Every `{@html}` in a component's tree. */
function htmlTags(node, found = []) {
  if (Array.isArray(node)) for (const child of node) htmlTags(child, found)
  else if (node !== null && typeof node === 'object') {
    if (node.type === 'HtmlTag') found.push(node)
    for (const [key, child] of Object.entries(node)) if (key !== 'parent') htmlTags(child, found)
  }
  return found
}

for (const path of files.filter((file) => file.endsWith('.svelte'))) {
  let tree
  try {
    tree = parse(readFileSync(path, 'utf8'), { modern: true })
  } catch (error) {
    breaches.push(`${name(path)}: doesn't parse: ${error instanceof Error ? error.message : String(error)}`)
    continue
  }
  if (path !== html && htmlTags(tree.fragment).length > 0) breaches.push(`${name(path)}: uses {@html}, which only Markdown.svelte may`)
}

if (breaches.length > 0) {
  console.error('The chat view must stand alone (see src/chat/index.ts):')
  for (const breach of [...new Set(breaches)]) console.error(`  ${breach}`)
  process.exit(1)
}
console.log(`src/chat/ stands alone: ${code.length} files built, ${files.length} checked.`)

// Checks the chat view can be lifted out whole, and stays safe:
//
// - **Imports, as built:** every file in src/chat/ is built with Vite, as
//   the app builds it, and each import a file there makes, once resolved,
//   must be a file in src/chat/ or in one of the packages it may use:
//   @clackatoo/caf, Svelte and markdown-it. So the check follows what the
//   bundler does (template literals, `import.meta.glob`, odd paths such as
//   `svelte/../../x`). It never imports anything dynamically, so any
//   `import()` of its own fails too.
// - **Imports, as written:** the bundler never sees what TypeScript drops
//   (`import type`, `export type … from`, `import('x').T` in a type), so
//   each file is also parsed, with TypeScript's parser or Svelte's, and
//   every import in it held to the same rule. `import.meta.url` is
//   refused, since `new URL(…, import.meta.url)` reaches files without
//   importing them.
// - **CSS** in src/chat/ (every .css file, imported or not, and `<style>`)
//   may not `@import` or `url()` anything: Vite resolves those itself, out
//   of a plugin's sight.
// - **HTML put in the page as it is** happens only in Markdown.svelte,
//   which renders Markdown with HTML off, with `{@html}`. Anywhere else,
//   `{@html}`, `bind:innerHTML`/`outerHTML`, `srcdoc`, and the DOM's
//   `innerHTML`, `outerHTML`, `insertAdjacentHTML`,
//   `createContextualFragment`, `setHTMLUnsafe`, `parseHTMLUnsafe`,
//   `DOMParser`'s `parseFromString` and `document.write` (on any object's
//   `document`), by name or by string, fail the check.
// - **TypeScript only:** a .js file in src/chat/ fails, since JSDoc can
//   import out of this check's sight. Triple-slash references count as
//   imports.
//
// Run in CI: `npm run check:imports`. It exits non-zero on any breach. It
// takes another folder to check instead of this app's, which its tests use.

import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { parse } from 'svelte/compiler'
import ts from 'typescript'
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

/** DOM names that put HTML in the page as it is. */
const sinks = new Set([
  'innerHTML',
  'outerHTML',
  'insertAdjacentHTML',
  'createContextualFragment',
  'srcdoc',
  'setHTMLUnsafe',
  'parseHTMLUnsafe',
  // DOMParser's: the nodes it makes come alive when put in the page.
  'parseFromString',
])

/** An import as written in `file`, held to the same rule as a resolved one. */
function written(file, specifier) {
  const from = name(file)
  if (specifier.startsWith('.') || specifier.startsWith('/')) {
    if (!inside(chat, resolve(dirname(file), bare(specifier)))) breaches.push(`${from}: imports ${specifier}, outside src/chat/`)
  } else if (!allowed.has(packageOf(specifier))) {
    breaches.push(`${from}: imports ${specifier}, which isn't @clackatoo/caf, Svelte or markdown-it`)
  } else if (specifier.split('/').includes('..')) {
    breaches.push(`${from}: imports ${specifier}, which climbs out of its package`)
  }
}

function css(file, source) {
  const plain = source.replace(/\/\*[\s\S]*?\*\//g, '')
  if (/@import\b/i.test(plain)) breaches.push(`${name(file)}: CSS @import, which Vite resolves out of this check's sight`)
  if (/\burl\s*\(/i.test(plain)) breaches.push(`${name(file)}: CSS url(), which may load anything`)
}

/** `document`, or anything's `.document`, such as `window.document`. */
function isDocument(node) {
  return (ts.isIdentifier(node) && node.text === 'document') || (ts.isPropertyAccessExpression(node) && node.name.text === 'document')
}

/** A script, with TypeScript's parser. */
function script(file, source) {
  const tree = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
  // `/// <reference path=…>`, relative to the file however it's written,
  // and `/// <reference types=…>`, a package.
  for (const reference of tree.referencedFiles) {
    written(file, /^[./]/.test(reference.fileName) ? reference.fileName : `./${reference.fileName}`)
  }
  for (const reference of tree.typeReferenceDirectives) written(file, reference.fileName)
  const visit = (node) => {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteralLike(node.moduleSpecifier)) {
      written(file, node.moduleSpecifier.text)
    } else if (ts.isImportTypeNode(node) && ts.isLiteralTypeNode(node.argument) && ts.isStringLiteralLike(node.argument.literal)) {
      written(file, node.argument.literal.text)
    } else if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) {
      const [first] = node.arguments
      if (first && ts.isStringLiteralLike(first)) written(file, first.text)
    } else if (ts.isPropertyAccessExpression(node) && ts.isMetaProperty(node.expression) && node.name.text === 'url') {
      breaches.push(`${name(file)}: uses import.meta.url, which reaches files without importing them`)
    } else if (ts.isPropertyAccessExpression(node) && /^write(ln)?$/.test(node.name.text) && isDocument(node.expression)) {
      breaches.push(`${name(file)}: uses document.write, which puts HTML in the page as it is`)
    } else if ((ts.isIdentifier(node) || ts.isPrivateIdentifier(node) || ts.isStringLiteralLike(node)) && sinks.has(node.text)) {
      breaches.push(`${name(file)}: uses ${node.text}, which puts HTML in the page as it is`)
    }
    ts.forEachChild(node, visit)
  }
  visit(tree)
}

/** A component, with Svelte's parser: its scripts, its markup and its style. */
function component(file, source) {
  let tree
  try {
    tree = parse(source, { modern: true })
  } catch (error) {
    breaches.push(`${name(file)}: doesn't parse: ${error instanceof Error ? error.message : String(error)}`)
    return
  }
  const from = name(file)
  const literal = (node) => (node?.type === 'Literal' && typeof node.value === 'string' ? node.value : null)
  const visit = (node) => {
    if (Array.isArray(node)) return node.forEach(visit)
    if (node === null || typeof node !== 'object') return
    switch (node.type) {
      case 'HtmlTag':
        if (file !== html) breaches.push(`${from}: uses {@html}, which only Markdown.svelte may`)
        break
      case 'BindDirective':
        if (/^(inner|outer)HTML$/.test(node.name)) breaches.push(`${from}: uses bind:${node.name}, which puts HTML in the page as it is`)
        break
      case 'Attribute':
        if (sinks.has(node.name)) breaches.push(`${from}: uses ${node.name}, which puts HTML in the page as it is`)
        break
      case 'ImportDeclaration':
      case 'ExportNamedDeclaration':
      case 'ExportAllDeclaration':
      case 'ImportExpression':
        if (literal(node.source) !== null) written(file, literal(node.source))
        break
      case 'TSImportType': {
        const argument = node.argument?.literal ?? node.argument
        if (literal(argument) !== null) written(file, literal(argument))
        break
      }
      case 'MemberExpression':
        if (node.object?.type === 'MetaProperty' && node.property?.name === 'url') {
          breaches.push(`${from}: uses import.meta.url, which reaches files without importing them`)
        }
        if (
          /^write(ln)?$/.test(node.property?.name ?? '') &&
          ((node.object?.type === 'Identifier' && node.object.name === 'document') ||
            (node.object?.type === 'MemberExpression' && node.object.property?.name === 'document'))
        ) {
          breaches.push(`${from}: uses document.write, which puts HTML in the page as it is`)
        }
        break
      case 'Identifier':
        if (sinks.has(node.name)) breaches.push(`${from}: uses ${node.name}, which puts HTML in the page as it is`)
        break
      case 'Literal':
        if (sinks.has(node.value)) breaches.push(`${from}: uses ${node.value}, which puts HTML in the page as it is`)
        break
      case 'StyleSheet':
        if (typeof node.content?.styles === 'string') css(file, node.content.styles)
        break
    }
    for (const [key, child] of Object.entries(node)) if (key !== 'parent' && key !== 'metadata') visit(child)
  }
  visit(tree)
}

for (const file of files) {
  const source = readFileSync(file, 'utf8')
  if (file.endsWith('.svelte')) component(file, source)
  else if (file.endsWith('.css')) css(file, source)
  // JavaScript could import in JSDoc, out of this check's sight: the chat
  // view is TypeScript.
  else if (/\.[cm]?jsx?$/.test(file)) breaches.push(`${name(file)}: a .js file; the chat view is TypeScript`)
  else if (/\.[cm]?tsx?$/.test(file)) script(file, source)
}

if (breaches.length > 0) {
  console.error('The chat view must stand alone (see src/chat/index.ts):')
  for (const breach of [...new Set(breaches)]) console.error(`  ${breach}`)
  process.exit(1)
}
console.log(`src/chat/ stands alone: ${code.length} files built, ${files.length} checked.`)

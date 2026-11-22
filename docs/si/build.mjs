// Builds what Learn shows of CAF SI from the mock-ups in this folder:
//
// - a screenshot of every state each page draws, in the dark theme, beside
//   the Standard Interface articles (`<page>-<state>.webp`, lossless, less
//   than half a PNG's size);
// - each page bundled into one self-contained file, its `si.css` and
//   `si.js` written in, published on Learn as an attachment, so an article
//   can link to it live (`docs/learn/si-live/<page>.html`).
//
// Each page says what to take with `SI.shots`, set after it draws:
//
//   SI.shots = {
//     target: '#pane',                 // the element a state is drawn in
//     states: ['typing', 'running'],   // the states, by id, in order
//     show: id => show(id),            // draws one
//     css: '.pane { height: 460px }',   // a stylesheet for the pictures, optional
//     // other parts, optional: drawn in `state` if it's given
//     extras: [{ id: 'narrow', target: '#narrow', state: 'typing' }],
//   }
//
// Run it from the repository's root after changing a mock-up, and check in
// what it writes:
//
//   node docs/si/build.mjs                # every page
//   node docs/si/build.mjs compose usage  # some
//
// It uses the debugger's Playwright (run `npm ci` in `ts/` first) and the
// Chromium its end-to-end tests use (`CAF_E2E_CHROMIUM`, or Playwright's
// own), and Python's Pillow to write WebP: Chromium's own lossless WebP is
// bigger than a PNG. Screenshots differ a little between machines' fonts, so
// nothing checks them in CI: regenerate them all at once, on one machine.
//
// A page that throws, or logs an error, fails the build, and so does one
// whose bundle would still load anything from beside it. A page's pictures
// are only replaced once all of them were taken.

import { execFileSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, '..', '..')
const { chromium } = createRequire(join(root, 'ts', 'debugger', 'package.json'))('playwright')

/** The pages, by file name without `.html`, and the name their pictures,
 *  and their article on Learn, take. */
const PAGES = {
  compose: 'composer',
  transcript: 'transcript',
  agents: 'agents',
  chats: 'chats',
  usage: 'usage',
}

/** Where the Standard Interface's articles are on Learn. */
const ARTICLES = '/caf/standard-interface'

const SHOTS = join(root, 'docs', 'learn', '2--developer-tools.shelf', 'caf.product', '3--standard-interface.topic')
const LIVE = join(root, 'docs', 'learn', 'si-live')

/** `file`, with the stylesheet and script it links to written into it. */
function bundled(file) {
  const css = readFileSync(join(here, 'si.css'), 'utf8')
  const js = readFileSync(join(here, 'si.js'), 'utf8')
  const link = '<link rel="stylesheet" href="si.css">', script = '<script src="si.js"></script>'
  const page = readFileSync(join(here, file), 'utf8')
  if (!page.includes(link) || !page.includes(script)) throw new Error(`${file} doesn't link si.css and si.js as ${link} and ${script}`)
  const out = page
    .replace(link, () => `<style>\n${css}</style>`)
    .replace(script, () => `<script>\n${js}</script>`)
    // A bundle is published on its own, so its links to the other pages go
    // to their articles instead.
    .replace(/href="([a-z]+)\.html(#[^"]*)?"/g, (all, page, hash = '') =>
      page === 'index' ? `href="${ARTICLES}/overview${hash}"` : Object.hasOwn(PAGES, page) ? `href="${ARTICLES}/${PAGES[page]}${hash}"` : all)
  // Nothing loaded from anywhere: a relative or remote src, a stylesheet
  // or script linked, a CSS url() but data:, or an @import.
  const left = out.match(/\ssrc=["'][^"']*["']|<link\b[^>]*rel=["']?stylesheet|@import\b|url\(\s*["']?(?!data:)[^)"']+|href=["'](?!https?:|\/|#)[^"']*["']/)
  if (left) throw new Error(`${file}'s bundle would still load something from beside it: ${left[0].trim()}`)
  return out
}

/** Writes `png`, a screenshot, to `path` as lossless WebP. */
function webp(png, path) {
  const convert = 'import io, sys\nfrom PIL import Image\nImage.open(io.BytesIO(sys.stdin.buffer.read())).save(sys.argv[1], lossless=True, method=6)'
  execFileSync('python3', ['-I', '-c', convert, path], { input: png })
}

/** A state's id as a picture's name: Trail wants lowercase slugs, so
 *  `signedIn` is `signed-in`. */
const slug = id => id.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase()

const asked = process.argv.slice(2)
const pages = asked.length ? asked : Object.keys(PAGES)
for (const page of pages) if (!Object.hasOwn(PAGES, page)) throw new Error(`no page '${page}': it's one of ${Object.keys(PAGES).join(', ')}`)

mkdirSync(SHOTS, { recursive: true })
mkdirSync(LIVE, { recursive: true })
const browser = await chromium.launch({ executablePath: process.env.CAF_E2E_CHROMIUM || undefined })
try {
  const context = await browser.newContext({ colorScheme: 'dark', deviceScaleFactor: 2, viewport: { width: 1400, height: 1000 } })
  for (const page of pages) {
    const name = PAGES[page]
    const live = bundled(`${page}.html`)
    const tab = await context.newPage()
    const errors = []
    tab.on('pageerror', error => errors.push(error.message))
    tab.on('console', message => message.type() === 'error' && errors.push(message.text()))
    const failed = () => {
      if (errors.length) throw new Error(`${page}.html went wrong: ${errors.join('; ')}`)
    }
    await tab.goto(pathToFileURL(join(here, `${page}.html`)).href)
    const shots = await tab.evaluate(() => window.SI?.shots && { target: SI.shots.target, states: SI.shots.states, extras: SI.shots.extras || [], css: SI.shots.css })
    if (!shots) throw new Error(`${page}.html doesn't say what to take: set SI.shots`)
    const names = [...shots.states, ...shots.extras.map(extra => extra.id)].map(slug)
    const twice = names.find((each, at) => names.indexOf(each) !== at)
    if (twice) throw new Error(`${page}.html names two pictures '${twice}'`)
    if (shots.css) await tab.addStyleTag({ content: shots.css })
    // Taken into a folder of their own, beside this script so they can be
    // moved in one step each, and moved in only once all are.
    const taken = mkdtempSync(join(here, '.taking-'))
    const take = async (target, id) => {
      // Let anything drawn on the next frame, and the fonts, settle.
      await tab.evaluate(() => document.fonts.ready.then(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done)))))
      failed()
      webp(await tab.locator(target).screenshot({ animations: 'disabled' }), join(taken, `${name}-${slug(id)}.webp`))
    }
    try {
      for (const state of shots.states) {
        await tab.evaluate(id => SI.shots.show(id), state)
        await take(shots.target, state)
      }
      for (const extra of shots.extras) {
        if (extra.state) await tab.evaluate(id => SI.shots.show(id), extra.state)
        await take(extra.target, extra.id)
      }
      failed()
      // Pictures of states a page no longer draws go with it.
      for (const old of readdirSync(SHOTS)) if (old.startsWith(`${name}-`) && /\.(png|webp)$/.test(old)) rmSync(join(SHOTS, old))
      for (const each of readdirSync(taken)) renameSync(join(taken, each), join(SHOTS, each))
      writeFileSync(join(LIVE, `${page}.html`), live)
    } finally {
      rmSync(taken, { recursive: true, force: true })
    }
    await tab.close()
    console.log(`${page}: ${shots.states.length + shots.extras.length} pictures, and si-live/${page}.html`)
  }
} finally {
  await browser.close()
}

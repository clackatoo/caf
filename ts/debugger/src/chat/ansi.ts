// A command's output in colour: its lines, as CAF keeps them (`Output`),
// with their SGR codes (`ESC [ … m`) turned into styles. Nothing else a
// terminal would act on is: any other escape, and every other control
// character, shows as `unhidden` shows it (`^[`), so output can never
// move or reorder what's drawn. Nor is text drawn on a background of its
// own colour: such a background is dropped. (Two colours merely close to
// each other can still be hard to read: that's the command's own doing.)
//
// A style comes only from the codes' numbers, read here, never from the
// output's own text: a span's style is one of the strings built below.

import { unhidden } from './unhidden.ts'

/** A run of a line in one style: `style` is CSS, or empty for none. */
export type Run = { text: string; style: string }

/** What the codes so far have set. */
type Pen = {
  bold: boolean
  dim: boolean
  italic: boolean
  underline: boolean
  strike: boolean
  inverse: boolean
  fg: string | null
  bg: string | null
}

const PLAIN: Pen = { bold: false, dim: false, italic: false, underline: false, strike: false, inverse: false, fg: null, bg: null }

/** The 16 colours, as the view's CSS names them: the output's frame sets them for its theme. */
function named(n: number): string {
  return `var(--caf-ansi-${n})`
}

/** One of the 256 colours: the 16, a 6×6×6 cube, then 24 greys. */
function indexed(n: number): string {
  if (n < 16) return named(n)
  if (n < 232) {
    const level = (k: number) => (k === 0 ? 0 : 55 + k * 40)
    const c = n - 16
    return rgb(level(Math.floor(c / 36)), level(Math.floor(c / 6) % 6), level(c % 6))
  }
  const grey = 8 + (n - 232) * 10
  return rgb(grey, grey, grey)
}

function rgb(r: number, g: number, b: number): string {
  return `rgb(${r}, ${g}, ${b})`
}

const byte = (n: number | undefined): n is number => n !== undefined && Number.isInteger(n) && n >= 0 && n <= 255

/**
 * The parameters of an SGR sequence, as groups: each is one parameter
 * and its sub-parameters, which `:` separates and `;` never does, as in
 * `38:2::255:0:0` (a colour in one group) beside `38;2;255;0;0` (the same
 * in five). An empty one is 0.
 */
type Group = number[]

/** The group at `at`, if it's a lone number: what `38;5;n` and `38;2;r;g;b` take after them. */
function lone(groups: readonly Group[], at: number): number | undefined {
  const group = groups[at]
  return group !== undefined && group.length === 1 ? group[0] : undefined
}

/**
 * An extended colour from `38` or `48`'s parameters after it, `5;n` or
 * `2;r;g;b`. Returns the colour, or null for one that makes no sense, and
 * how many groups it took.
 */
function extended(groups: readonly Group[], at: number): { colour: string | null; took: number } {
  const how = lone(groups, at)
  if (how === 5) {
    const n = lone(groups, at + 1)
    return { colour: byte(n) ? indexed(n) : null, took: 2 }
  }
  if (how === 2) {
    const [r, g, b] = [lone(groups, at + 1), lone(groups, at + 2), lone(groups, at + 3)]
    return { colour: byte(r) && byte(g) && byte(b) ? rgb(r, g, b) : null, took: 4 }
  }
  return { colour: null, took: 1 }
}

/**
 * An extended colour in one group, after its `38` or `48`: `5:n`, or
 * `2:r:g:b` with any colour space's id before them, as `2::r:g:b` or
 * `2:0:r:g:b`. Null for one that makes no sense.
 */
function subColour(sub: readonly number[]): string | null {
  if (sub[0] === 5 && sub.length === 2) return byte(sub[1]) ? indexed(sub[1]) : null
  if (sub[0] === 2 && (sub.length === 4 || sub.length === 5)) {
    const [r, g, b] = sub.slice(-3)
    return byte(r) && byte(g) && byte(b) ? rgb(r, g, b) : null
  }
  return null
}

/** The pen after an SGR sequence's parameters. */
function apply(pen: Pen, groups: readonly Group[]): Pen {
  const next = { ...pen }
  if (groups.length === 0) return { ...PLAIN }
  for (let i = 0; i < groups.length; i++) {
    const group = groups[i]!
    const p = group[0]!
    if (group.length > 1) {
      // A parameter with sub-parameters: a colour, or an underline's
      // style (`4:0` none, `4:3` curly, drawn as any underline). Any
      // other is left alone, whole.
      const sub = group.slice(1)
      if (p === 38) next.fg = subColour(sub) ?? next.fg
      else if (p === 48) next.bg = subColour(sub) ?? next.bg
      else if (p === 4 && sub.length === 1) next.underline = sub[0] !== 0
      continue
    }
    if (p === 0) Object.assign(next, PLAIN)
    else if (p === 1) next.bold = true
    else if (p === 2) next.dim = true
    else if (p === 3) next.italic = true
    else if (p === 4) next.underline = true
    else if (p === 7) next.inverse = true
    else if (p === 9) next.strike = true
    else if (p === 22) next.bold = next.dim = false
    else if (p === 23) next.italic = false
    else if (p === 24) next.underline = false
    else if (p === 27) next.inverse = false
    else if (p === 29) next.strike = false
    else if (p >= 30 && p <= 37) next.fg = named(p - 30)
    else if (p === 39) next.fg = null
    else if (p >= 40 && p <= 47) next.bg = named(p - 40)
    else if (p === 49) next.bg = null
    else if (p >= 90 && p <= 97) next.fg = named(p - 90 + 8)
    else if (p >= 100 && p <= 107) next.bg = named(p - 100 + 8)
    else if (p === 38 || p === 48) {
      const { colour, took } = extended(groups, i + 1)
      if (p === 38) next.fg = colour ?? next.fg
      else next.bg = colour ?? next.bg
      i += took
    }
    // Anything else (blink, fonts, overline…) is left as it was.
  }
  return next
}

/** A pen as CSS. */
function css(pen: Pen): string {
  let fg = pen.fg
  let bg = pen.bg
  if (pen.inverse) [fg, bg] = [bg ?? 'var(--caf-inset)', fg ?? 'var(--caf-text)']
  // Text on its own colour can't be read: it keeps the view's background.
  if (bg !== null && bg === fg) bg = null
  const rules: string[] = []
  if (fg !== null) rules.push(`color: ${fg}`)
  if (bg !== null) rules.push(`background: ${bg}`)
  if (pen.bold) rules.push('font-weight: bold')
  if (pen.dim) rules.push('opacity: 0.7')
  if (pen.italic) rules.push('font-style: italic')
  const lines = [pen.underline && 'underline', pen.strike && 'line-through'].filter(Boolean)
  if (lines.length > 0) rules.push(`text-decoration: ${lines.join(' ')}`)
  return rules.join('; ')
}

// An SGR sequence: CSI, parameters of digits, `;` and `:`, then `m`.
const SGR = /\x1b\[([0-9;:]*)m/g

/**
 * Lines of output as runs of one style each, the pen carried from line to
 * line as a terminal carries it.
 */
export function colour(lines: readonly string[]): Run[][] {
  let pen = PLAIN
  return lines.map((line) => {
    const runs: Run[] = []
    const push = (text: string) => {
      if (text === '') return
      const shown = unhidden(text)
      const style = css(pen)
      const last = runs[runs.length - 1]
      if (last !== undefined && last.style === style) last.text += shown
      else runs.push({ text: shown, style })
    }
    let at = 0
    for (const match of line.matchAll(SGR)) {
      push(line.slice(at, match.index))
      const groups = match[1] === '' ? [] : match[1]!.split(';').map((group) => group.split(':').map((p) => (p === '' ? 0 : Number(p))))
      pen = apply(pen, groups)
      at = match.index + match[0].length
    }
    push(line.slice(at))
    return runs
  })
}

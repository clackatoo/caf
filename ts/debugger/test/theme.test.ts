// Text in the chat view's colours is readable: WCAG AA, 4.5:1, for each
// colour on each background it's drawn on, in both themes.

import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

const css = readFileSync(resolve(import.meta.dirname, '../src/chat/theme.css'), 'utf8')
const [light, dark] = css.split('@media (prefers-color-scheme: dark)')

function colours(block: string): Map<string, string> {
  return new Map([...block!.matchAll(/--caf-([a-z-]+):\s*(#[0-9a-f]{6});/gi)].map(([, name, hex]) => [name!, hex!]))
}

function luminance(hex: string): number {
  const channel = (i: number) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4
  }
  return 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (hi! + 0.05) / (lo! + 0.05)
}

/** Each text colour, and the backgrounds it's drawn on. */
const pairs: [string, string[]][] = [
  ['text', ['bg', 'panel', 'inset', 'user', 'warn-bg', 'error-bg', 'caf-bg', 'hover']],
  ['muted', ['bg', 'panel', 'inset', 'user', 'warn-bg', 'error-bg']],
  ['faint', ['bg', 'panel']],
  ['ok', ['bg', 'inset']],
  ['warn', ['bg', 'warn-bg']],
  ['error', ['bg', 'error-bg']],
  ['accent', ['bg', 'panel']],
  ['link', ['bg', 'panel']],
  ['caf', ['bg', 'caf-bg', 'error-bg']],
  ['accent-text', ['accent']],
]

for (const [theme, block] of [['light', light], ['dark', dark]] as const) {
  describe(`the ${theme} theme`, () => {
    const palette = colours(block!)
    for (const [text, backgrounds] of pairs) {
      for (const background of backgrounds) {
        it(`${text} on ${background} is readable`, () => {
          const ratio = contrast(palette.get(text)!, palette.get(background)!)
          expect(ratio).toBeGreaterThanOrEqual(4.5)
        })
      }
    }
  })
}

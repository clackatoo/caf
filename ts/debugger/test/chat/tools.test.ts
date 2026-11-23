// A tool call drawn whole: its status in CAF SI's colour roles, who
// allowed it, every kind of piece it gave back, the files it's about,
// its input, live output growing as CAF sends it, and a permission
// question showing the call it's about. Nothing an agent sends becomes
// markup, a followed link or a script.

import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { cleanup, fireEvent } from '@testing-library/svelte'
import { afterEach, describe, expect, it } from 'vitest'
import type { Decision, Piece, ToolCall, ToolStatus } from '@clackatoo/caf'
import { colour } from '../../src/chat/ansi.ts'
import { bytes, decisionText, endingText, followable, imageSource } from '../../src/chat/tools.ts'
import { chat, draw, item, question, turn } from './build.ts'

afterEach(cleanup)

const TOOL_CSS = readFileSync(resolve(import.meta.dirname, '../../src/chat/Tool.svelte'), 'utf8')

function call(fields: Partial<ToolCall> = {}): ToolCall {
  return {
    title: 'Run tests',
    kind: { type: 'Execute' },
    status: { type: 'Done' },
    origin: { type: 'Agent', value: 'Bash' },
    action: null,
    input: null,
    input_cut: null,
    result: [],
    locations: [],
    locations_cut: 0,
    decision: { type: 'NotAsked' },
    app: null,
    ...fields,
  }
}

/** The view with one turn of these calls, each row opened. */
function drawCalls(...calls: ToolCall[]) {
  const items = calls.map((value) => item({ type: 'Tool', value }))
  const view = draw(chat({ turns: [turn({ items })] }))
  for (const row of view.container.querySelectorAll('details.tool')) (row as HTMLDetailsElement).open = true
  return { ...view, items }
}

function rowOf(container: HTMLElement): HTMLElement {
  return container.querySelector('details.tool') as HTMLElement
}

function text(element: Element | null): string {
  return (element?.textContent ?? '').replace(/\s+/g, ' ').trim()
}

describe('a call\'s status', () => {
  const statuses: [ToolStatus['type'], string, string | null][] = [
    ['Pending', 'Waiting to start', null],
    ['Asking', 'Waiting for you', 'accent'],
    ['Running', 'Running', 'accent'],
    ['Done', 'Done', 'muted'],
    ['Failed', 'Failed', 'error'],
    ['Refused', 'Refused', 'error'],
    ['Cancelled', 'Cancelled', 'muted'],
  ]
  for (const [type, says, role] of statuses) {
    it(`says ${type} as "${says}", in the ${role ?? 'calm'} role`, () => {
      const { container } = drawCalls(call({ status: { type } as ToolStatus }))
      const row = rowOf(container)
      expect(row.dataset.status).toBe(type)
      expect(text(row.querySelector('.status'))).toBe(says)
      // The role is the stylesheet's: the rule naming this status's colour.
      const grouped = [...TOOL_CSS.matchAll(/((?:\[data-status='\w+'\] \.status,?\s*)+)\{\s*color: var\(--caf-(\w+)\)/g)]
        .filter((match) => match[1]!.includes(`'${type}'`))
        .map((match) => match[2])
      if (role === null) expect(grouped).toEqual([])
      else expect(grouped).toEqual([role])
    })
  }

  it('draws the call waiting for the user with an edge in the accent', () => {
    const { container } = drawCalls(call({ status: { type: 'Asking' } }))
    expect(rowOf(container).dataset.status).toBe('Asking')
  })
})

describe('who decided', () => {
  const decisions: [Decision, string | null][] = [
    [{ type: 'Allowed', value: { type: 'User', value: { option: 'Always allow' } } }, 'Allowed by you'],
    [{ type: 'Allowed', value: { type: 'Policy', value: { reason: 'reads in the project are fine' } } }, 'Allowed by the policy: reads in the project are fine'],
    [{ type: 'Allowed', value: { type: 'Policy', value: { reason: null } } }, 'Allowed by the policy'],
    [{ type: 'Refused', value: { type: 'User', value: { option: 'Reject' } } }, 'Refused by you'],
    [{ type: 'Refused', value: { type: 'Policy', value: { reason: 'outside the project' } } }, 'Refused by the policy: outside the project'],
    [{ type: 'Refused', value: { type: 'Policy', value: { reason: null } } }, 'Refused by the policy'],
    [{ type: 'NotAsked' }, null],
    [{ type: 'Undecided' }, null],
    [{ type: 'NotRecorded' }, null],
  ]
  for (const [decision, says] of decisions) {
    it(`says ${JSON.stringify(decision)} as ${JSON.stringify(says)}`, () => {
      expect(decisionText(decision)).toBe(says)
      const { container } = drawCalls(call({ decision }))
      const line = container.querySelector('.tool .decision')
      if (says === null) expect(line).toBeNull()
      else expect(text(line)).toBe(says)
    })
  }

  it('names someone it doesn\'t know by their tag', () => {
    expect(decisionText({ type: 'Allowed', value: { type: 'Oracle' } as never })).toBe(
      "Allowed, by someone this viewer doesn't know (Oracle)",
    )
  })
})

describe('what a call gave back', () => {
  function drawPieces(pieces: Piece[], status: ToolStatus['type'] = 'Done') {
    const { container, ...rest } = drawCalls(call({ result: pieces, status: { type: status } as ToolStatus }))
    return { container, row: rowOf(container), ...rest }
  }

  it('draws text, and says how much of it was kept when it was cut', () => {
    const { row } = drawPieces([
      { type: 'Text', value: { text: 'all of it', cut: null } },
      { type: 'Text', value: { text: 'the start', cut: { kept: 65536, total: 1258291 } } },
    ])
    const texts = row.querySelectorAll('.piece.text')
    expect(text(texts[0]!.querySelector('pre'))).toBe('all of it')
    expect(texts[0]!.querySelector('.cut')).toBeNull()
    expect(text(texts[1]!.querySelector('.cut'))).toBe('Kept 64 KB of 1.2 MB')
  })

  it('draws a diff: its file, counts, hunks with both numbers, and the hunks not kept', () => {
    const { row } = drawPieces([
      {
        type: 'Diff',
        value: {
          path: '/p/src/lib.rs',
          new_file: false,
          added: 7,
          removed: 1,
          more: 3,
          hunks: [
            {
              lines: [
                { change: { type: 'Kept' }, old: 9, new: 9, text: 'fn main() {' },
                { change: { type: 'Removed' }, old: 10, new: null, text: '    old();' },
                { change: { type: 'Added' }, old: null, new: 10, text: '    new();' },
              ],
            },
            { lines: [{ change: { type: 'Added' }, old: null, new: 40, text: '// end' }] },
          ],
        },
      },
    ])
    const diff = row.querySelector('.diff')!
    expect(text(diff.querySelector('.path'))).toBe('/p/src/lib.rs')
    expect(diff.querySelector('.made')).toBeNull()
    expect(text(diff.querySelector('.counts'))).toBe('+7 −1')
    expect(diff.querySelectorAll('table.hunk')).toHaveLength(2)
    const rows = [...diff.querySelectorAll('tr.line')].map((tr) =>
      [...tr.querySelectorAll('td')].map((td) => td.textContent),
    )
    expect(rows).toEqual([
      ['9', '9', ' ', 'fn main() {'],
      ['10', '', '−', '    old();'],
      ['', '10', '+', '    new();'],
      ['', '40', '+', '// end'],
    ])
    expect([...diff.querySelectorAll('tr.line')].map((tr) => (tr as HTMLElement).dataset.change)).toEqual([
      'Kept',
      'Removed',
      'Added',
      'Added',
    ])
    expect(text(diff.querySelector('.more'))).toBe('and 3 more hunks')
  })

  it('marks a new file, and says one more hunk in the singular', () => {
    const { row } = drawPieces([
      { type: 'Diff', value: { path: '/p/new.txt', new_file: true, added: 1, removed: 0, more: 1, hunks: [] } },
    ])
    expect(text(row.querySelector('.diff .made'))).toBe('New file')
    expect(text(row.querySelector('.diff .more'))).toBe('and 1 more hunk')
    expect(row.querySelector('.diff .hunks')).toBeNull()
  })

  it('draws output with its colours, the lines dropped, and how it ended', () => {
    const { row } = drawPieces([
      {
        type: 'Output',
        value: { lines: ['\x1b[32mok\x1b[0m plain', '\x1b[1;31mbold red'], dropped: 120, ending: { type: 'Exited', value: { type: 'Code', value: 0 } } },
      },
    ])
    const output = row.querySelector('.output')!
    expect(text(output.querySelector('.dropped'))).toBe('120 earlier lines not kept')
    expect(output.querySelector('.lines')!.textContent).toBe('ok plain\nbold red')
    const styled = [...output.querySelectorAll('.lines span[style]')].map((span) => [span.textContent, span.getAttribute('style')])
    expect(styled).toEqual([
      ['ok', 'color: var(--caf-ansi-2);'],
      ['bold red', 'color: var(--caf-ansi-1); font-weight: bold;'],
    ])
    expect(text(output.querySelector('.ending'))).toBe('Exited with code 0')
    expect(output.querySelector('.ending')!.classList.contains('bad')).toBe(false)
    expect(output.querySelector('.caret')).toBeNull()
  })

  it('says one earlier line in the singular, and a failed exit in the warning colour', () => {
    const { row } = drawPieces([
      { type: 'Output', value: { lines: ['x'], dropped: 1, ending: { type: 'Exited', value: { type: 'Code', value: 101 } } } },
    ], 'Failed')
    expect(text(row.querySelector('.output .dropped'))).toBe('1 earlier line not kept')
    expect(text(row.querySelector('.output .ending'))).toBe('Exited with code 101')
    expect(row.querySelector('.output .ending')!.classList.contains('bad')).toBe(true)
  })

  it('says each way a command can end', () => {
    expect(endingText({ type: 'Exited', value: { type: 'Signal', value: 9 } })).toBe('Killed by signal 9')
    expect(endingText({ type: 'Unknown' })).toBe('Ended, without saying how')
    expect(endingText({ type: 'Vanished' } as never)).toBe("Ended in a way this viewer doesn't know (Vanished)")
  })

  it('draws a caret while the command runs, and none once it has ended', () => {
    const { row } = drawPieces([{ type: 'Output', value: { lines: ['building'], dropped: 0, ending: null } }], 'Running')
    expect(row.querySelector('.output .caret')?.getAttribute('aria-label')).toBe('Still running')
    expect(row.querySelector('.output .ending')).toBeNull()
    cleanup()
    // Cut off with no ending: it isn't running any more, so no caret.
    const cut = drawPieces([{ type: 'Output', value: { lines: ['building'], dropped: 0, ending: null } }], 'Failed')
    expect(cut.row.querySelector('.output .caret')).toBeNull()
  })

  it('draws an image kept whole, and says when one was too big to keep', () => {
    const { row } = drawPieces([
      { type: 'Image', value: { mime: 'image/png', data: 'iVBORw0KGgo=', size: 2048 } },
      { type: 'Image', value: { mime: 'image/png', data: null, size: 3 * 1024 * 1024 } },
    ])
    const images = row.querySelectorAll('.piece.image')
    const img = images[0]!.querySelector('img')!
    expect(img.getAttribute('src')).toBe('data:image/png;base64,iVBORw0KGgo=')
    expect(img.getAttribute('alt')).toBe('An image (image/png, 2 KB)')
    expect(images[1]!.querySelector('img')).toBeNull()
    expect(text(images[1]!)).toBe('An image too big to keep (3 MB)')
  })

  it('never makes a data URI of something that isn\'t an image', () => {
    const { row } = drawPieces([
      { type: 'Image', value: { mime: 'text/html', data: 'PHNjcmlwdD4=', size: 8 } },
      { type: 'Image', value: { mime: 'image/png;x=1', data: 'AAAA', size: 3 } },
      { type: 'Image', value: { mime: 'image/png', data: 'AA"onerror="x', size: 3 } },
    ])
    expect(row.querySelector('img')).toBeNull()
    expect(text(row.querySelector('.piece.image'))).toBe("An image of a type this viewer doesn't show (text/html)")
    expect(imageSource('IMAGE/PNG', 'AAAA')).toBe('data:image/png;base64,AAAA')
  })

  it('draws a web link to follow, and any other address as text', () => {
    const { row } = drawPieces([
      { type: 'Link', value: { uri: 'https://example.com/docs', name: 'The docs' } },
      { type: 'Link', value: { uri: 'javascript:alert(1)', name: 'Click me' } },
      { type: 'Link', value: { uri: 'file:///p/src/lib.rs', name: 'lib.rs' } },
      { type: 'Link', value: { uri: 'data:text/html,<script>', name: 'data' } },
    ])
    const links = row.querySelectorAll('.piece.link')
    const followed = links[0]!.querySelector('a')!
    expect(followed.getAttribute('href')).toBe('https://example.com/docs')
    expect(followed.getAttribute('rel')).toBe('noopener noreferrer')
    expect(followed.textContent).toBe('The docs')
    for (const link of [...links].slice(1)) expect(link.querySelector('a')).toBeNull()
    expect(text(links[1]!)).toBe('Click me javascript:alert(1)')
    expect(text(links[2]!)).toBe('lib.rs file:///p/src/lib.rs')
    expect(row.querySelectorAll('a')).toHaveLength(1)
    expect(followable('JavaScript:alert(1)')).toBeNull()
    expect(followable(' javascript:alert(1)')).toBeNull()
    expect(followable('not a url')).toBeNull()
  })

  it('draws a piece CAF couldn\'t read by what it said it was, and one this viewer doesn\'t know by its tag', () => {
    const { row } = drawPieces([
      { type: 'Unknown', value: { what: 'resource', raw: { uri: 'x' }, summary: null } },
      { type: 'Hologram', value: {} } as never,
    ])
    const unknown = row.querySelectorAll('.piece.unknown')
    expect(text(unknown[0]!.querySelector('summary'))).toBe("A piece this viewer can't show: resource")
    expect(text(unknown[1]!.querySelector('summary'))).toBe("A piece of a kind this viewer doesn't know: Hologram")
  })

  it('draws no markup an agent sends, in any piece', () => {
    const evil = '<img src=x onerror=alert(1)>'
    const { row } = drawPieces([
      { type: 'Text', value: { text: evil, cut: null } },
      { type: 'Output', value: { lines: [`\x1b[31m${evil}`, '\x1b]0;title\x07\x1b[2Jcleared'], dropped: 0, ending: null } },
      { type: 'Diff', value: { path: evil, new_file: false, added: 1, removed: 0, more: 0, hunks: [{ lines: [{ change: { type: 'Added' }, old: null, new: 1, text: evil }] }] } },
      { type: 'Link', value: { uri: evil, name: evil } },
    ], 'Running')
    expect(row.querySelector('img')).toBeNull()
    expect(row.querySelector('[onerror]')).toBeNull()
    // Escapes other than colours show as they are, never acted on.
    expect(row.querySelector('.output .lines')!.textContent).toContain('^[]0;title^G^[[2Jcleared')
  })
})

describe('colour in output', () => {
  const style = (line: string) => colour([line])[0]!.map((run) => [run.text, run.style])

  it('reads the 16 colours, bright ones, backgrounds and resets', () => {
    expect(style('\x1b[31ma\x1b[39mb')).toEqual([['a', 'color: var(--caf-ansi-1)'], ['b', '']])
    expect(style('\x1b[91ma')).toEqual([['a', 'color: var(--caf-ansi-9)']])
    expect(style('\x1b[44ma\x1b[49mb')).toEqual([['a', 'background: var(--caf-ansi-4)'], ['b', '']])
    expect(style('\x1b[103ma')).toEqual([['a', 'background: var(--caf-ansi-11)']])
    expect(style('\x1b[1;3;4ma\x1b[0mb')).toEqual([['a', 'font-weight: bold; font-style: italic; text-decoration: underline'], ['b', '']])
    expect(style('\x1b[1ma\x1b[mb')).toEqual([['a', 'font-weight: bold'], ['b', '']])
  })

  it('reads 256 colours and true colour, and ignores ones out of range', () => {
    expect(style('\x1b[38;5;196ma')).toEqual([['a', 'color: rgb(255, 0, 0)']])
    expect(style('\x1b[38;5;3ma')).toEqual([['a', 'color: var(--caf-ansi-3)']])
    expect(style('\x1b[48;5;232ma')).toEqual([['a', 'background: rgb(8, 8, 8)']])
    expect(style('\x1b[38;2;10;20;30ma')).toEqual([['a', 'color: rgb(10, 20, 30)']])
    expect(style('\x1b[38:2:10:20:30ma')).toEqual([['a', 'color: rgb(10, 20, 30)']])
    expect(style('\x1b[38;2;999;0;0ma')).toEqual([['a', '']])
    expect(style('\x1b[38;5;256ma')).toEqual([['a', '']])
  })

  it('carries the colour from one line to the next, as a terminal does', () => {
    expect(colour(['\x1b[32mgreen', 'still green\x1b[0m', 'plain']).map((runs) => runs.map((run) => run.style))).toEqual([
      ['color: var(--caf-ansi-2)'],
      ['color: var(--caf-ansi-2)'],
      [''],
    ])
  })

  it('shows every other escape and control character, never acting on it', () => {
    expect(style('a\x1b[2Kb\x1b[1Ac\rd‮e')).toEqual([['a^[[2Kb^[[1Ac^Md<U+202E>e', '']])
  })
})

describe('the files a call is about, and its input', () => {
  it('lists its places, and how many more there were', () => {
    const { container } = drawCalls(
      call({ locations: [{ path: '/p/a.rs', line: 12 }, { path: '/p/b.rs', line: null }], locations_cut: 4 }),
    )
    const items = [...container.querySelectorAll('.tool .locations li')].map((li) => li.textContent)
    expect(items).toEqual(['/p/a.rs:12', '/p/b.rs', 'and 4 more'])
  })

  it('lists none when it has none', () => {
    const { container } = drawCalls(call())
    expect(container.querySelector('.tool .locations')).toBeNull()
  })

  it('shows a cut input as the start of its JSON, and how much was kept', () => {
    const { container } = drawCalls(call({ input: '{"command":"echo aaaa', input_cut: { kept: 20, total: 200000 } }))
    const row = rowOf(container)
    expect(row.querySelector('pre')!.textContent).toBe('{"command":"echo aaaa')
    expect(text(row.querySelector('.cut'))).toBe('Kept 20 B of 195 KB')
  })

  it('shows a whole input laid out', () => {
    const { container } = drawCalls(call({ input: { command: 'ls' } }))
    const row = rowOf(container)
    expect(JSON.parse(row.querySelector('pre')!.textContent!)).toEqual({ command: 'ls' })
    expect(row.querySelector('.cut')).toBeNull()
  })

  it('says sizes short', () => {
    expect(bytes(0)).toBe('0 B')
    expect(bytes(1023)).toBe('1,023 B')
    expect(bytes(1536)).toBe('1.5 KB')
    expect(bytes(64 * 1024)).toBe('64 KB')
    expect(bytes(2 * 1024 * 1024)).toBe('2 MB')
  })
})

describe('live output', () => {
  it('grows as CAF sends more, and unfolds by itself while it runs', async () => {
    const running = (lines: string[], ending: null | { type: 'Unknown' }) =>
      call({ status: { type: ending === null ? 'Running' : 'Done' }, result: [{ type: 'Output', value: { lines, dropped: 0, ending } }] })
    const items = [item({ type: 'Tool', value: running(['one'], null) })]
    const running0 = turn({ life: { type: 'Running' }, items })
    const view = draw(chat({ turns: [running0] }))
    const row = rowOf(view.container) as HTMLDetailsElement
    await Promise.resolve()
    expect(row.open).toBe(true)
    expect(row.querySelector('.output .lines')!.textContent).toBe('one▍')
    const grown = { ...items[0]!, kind: { type: 'Tool' as const, value: running(['one', 'two'], null) } }
    await view.rerender({ chat: chat({ turns: [{ ...running0, items: [grown] }] }) })
    expect(rowOf(view.container).querySelector('.output .lines')!.textContent).toBe('one\ntwo▍')
    const done = { ...items[0]!, kind: { type: 'Tool' as const, value: running(['one', 'two'], { type: 'Unknown' }) } }
    await view.rerender({ chat: chat({ turns: [{ ...running0, life: { type: 'Finished' }, items: [done] }] }) })
    const ended = rowOf(view.container)
    expect(ended.querySelector('.output .lines')!.textContent).toBe('one\ntwo')
    expect(text(ended.querySelector('.output .ending'))).toBe('Ended, without saying how')
  })

  it('stays folded once the user folds it', async () => {
    const value = call({ status: { type: 'Running' }, result: [{ type: 'Output', value: { lines: ['a'], dropped: 0, ending: null } }] })
    const items = [item({ type: 'Tool', value })]
    const running = turn({ life: { type: 'Running' }, items })
    const view = draw(chat({ turns: [running] }))
    const row = rowOf(view.container) as HTMLDetailsElement
    await Promise.resolve()
    expect(row.open).toBe(true)
    row.open = false
    await fireEvent(row, new Event('toggle'))
    const grown = { ...items[0]!, kind: { type: 'Tool' as const, value: { ...value, result: [{ type: 'Output' as const, value: { lines: ['a', 'b'], dropped: 0, ending: null } }] } } }
    await view.rerender({ chat: chat({ turns: [{ ...running, items: [grown] }] }) })
    expect(rowOf(view.container).querySelector('.output .lines')!.textContent).toBe('a\nb▍')
    expect((rowOf(view.container) as HTMLDetailsElement).open).toBe(false)
  })
})

describe('a permission question about a call', () => {
  const options = [
    { id: 'y', label: 'Allow', kind: { type: 'AllowOnce' as const } },
    { id: 'n', label: 'Reject', kind: { type: 'RejectOnce' as const } },
  ]

  it('shows the command it would run', () => {
    const tool = item({
      type: 'Tool',
      value: call({
        status: { type: 'Asking' },
        decision: { type: 'Undecided' },
        origin: { type: 'Caf', value: { type: 'Bash' } },
        action: { type: 'Run', value: { command: 'cargo test', folder: '/p', background: false } },
      }),
    })
    const { container } = draw(
      chat({
        turns: [turn({ life: { type: 'Running' }, items: [tool] })],
        questions: [question({ type: 'Permission', value: { tool: tool.id, title: 'Run cargo test', options } })],
      }),
    )
    const asked = container.querySelector('.questions .question')!
    expect(asked.querySelector('.command')?.textContent).toBe('cargo test')
    expect(text(asked.querySelector('.folder'))).toBe('In /p')
  })

  it('shows the change it would make', () => {
    const tool = item({
      type: 'Tool',
      value: call({
        kind: { type: 'Edit' },
        status: { type: 'Asking' },
        decision: { type: 'Undecided' },
        origin: { type: 'Caf', value: { type: 'Edit' } },
        action: { type: 'Edit', value: { path: '/p/a.rs' } },
        result: [
          { type: 'Diff', value: { path: '/p/a.rs', new_file: false, added: 1, removed: 1, more: 0, hunks: [{ lines: [{ change: { type: 'Removed' }, old: 1, new: null, text: 'a' }, { change: { type: 'Added' }, old: null, new: 1, text: 'b' }] }] } },
        ],
      }),
    })
    const { container } = draw(
      chat({
        turns: [turn({ life: { type: 'Running' }, items: [tool] })],
        questions: [question({ type: 'Permission', value: { tool: tool.id, title: 'Edit a.rs', options } })],
      }),
    )
    const asked = container.querySelector('.questions .question')!
    expect(text(asked.querySelector('.diff .path'))).toBe('/p/a.rs')
    expect(text(asked.querySelector('.diff .counts'))).toBe('+1 −1')
    expect(asked.querySelector('.command')).toBeNull()
  })

  it('shows only its title before CAF has seen the call, or for a call with nothing to show', () => {
    const tool = item({ type: 'Tool', value: call({ status: { type: 'Asking' } }) })
    const { container } = draw(
      chat({
        turns: [turn({ life: { type: 'Running' }, items: [tool] })],
        questions: [
          question({ type: 'Permission', value: { tool: null, title: 'Unseen', options } }),
          question({ type: 'Permission', value: { tool: tool.id, title: 'Plain', options } }),
          question({ type: 'Permission', value: { tool: 999_999, title: 'Gone', options } }),
        ],
      }),
    )
    expect(container.querySelectorAll('.questions .about')).toHaveLength(0)
    expect(container.querySelectorAll('.questions .question')).toHaveLength(3)
  })
})

// A tool call drawn whole: its status in CAF SI's colour roles, who
// allowed it, every kind of piece it gave back, the files it's about,
// its input, live output growing as CAF sends it, and a permission
// question showing the call it's about. Nothing an agent sends becomes
// markup, a followed link or a script.

import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { cleanup, fireEvent } from '@testing-library/svelte'
import { afterEach, describe, expect, it } from 'vitest'
import { apply, type Change, type Chat, type Decision, type Piece, type ToolCall, type ToolStatus } from '@clackatoo/caf'
import { colour } from '../../src/chat/ansi.ts'
import { bytes, cafAbout, cafAsks, decisionText, endingText, followable, imageShown, jobNotice, linkName } from '../../src/chat/tools.ts'
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
    stopped_by: null,
    job: null,
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
    ['Asking', 'Waiting for you ↓', 'accent'],
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

describe('a call opened', () => {
  const edit: Piece = {
    type: 'Diff',
    value: { path: '/p/a.rs', new_file: false, added: 1, removed: 0, more: 0, hunks: [{ lines: [{ change: { type: 'Added' }, old: null, new: 1, text: 'b' }] }] },
  }

  it('opens by itself, once, to an edit\'s diff, as an edit is what the user most wants to check', async () => {
    const items = [item({ type: 'Tool', value: call({ kind: { type: 'Edit' }, result: [edit] }) })]
    const done = turn({ items })
    const view = draw(chat({ turns: [done] }))
    const row = rowOf(view.container) as HTMLDetailsElement
    await Promise.resolve()
    expect(row.open).toBe(true)
    row.open = false
    await fireEvent(row, new Event('toggle'))
    await view.rerender({ chat: chat({ turns: [{ ...done }] }) })
    expect((rowOf(view.container) as HTMLDetailsElement).open).toBe(false)
  })

  it('stays folded with no diff and nothing running', async () => {
    const view = draw(chat({ turns: [turn({ items: [item({ type: 'Tool', value: call({ result: [{ type: 'Text', value: { text: 'x', cut: null } }] }) })] })] }))
    await Promise.resolve()
    expect((rowOf(view.container) as HTMLDetailsElement).open).toBe(false)
  })

  it('says a refused call\'s result is what it would have done, which never happened', () => {
    const refused = drawCalls(call({ status: { type: 'Refused' }, result: [edit], decision: { type: 'Refused', value: { type: 'Policy', value: { reason: 'outside the project' } } } }))
    const labels = [...rowOf(refused.container).querySelectorAll('.label')].map((label) => text(label))
    expect(labels[0]).toBe('What it would have done (never ran)')
    cleanup()
    const done = drawCalls(call({ result: [edit] }))
    expect(text(rowOf(done.container).querySelector('.label'))).toBe('Output')
  })

  it('draws who decided muted, whatever they decided, as CAF SI says', () => {
    expect(TOOL_CSS).toMatch(/\.decision \{[^}]*color: var\(--caf-muted\)/)
    expect(TOOL_CSS).not.toMatch(/\.decision\[/)
  })

  it('lays it out as CAF SI does: who decided, what it gave back, its files, then its input, folded away', () => {
    const { container } = drawCalls(
      call({ input: { command: 'ls' }, result: [{ type: 'Text', value: { text: 'x', cut: null } }], locations: [{ path: '/p/a', line: null }], decision: { type: 'Allowed', value: { type: 'User', value: { option: 'Allow' } } } }),
    )
    const row = rowOf(container)
    const order = [...row.querySelectorAll('.body > .decision, .body > .label, .body > details.input > summary')].map((part) => text(part))
    expect(order).toEqual(['Allowed by you', 'Output', 'Files', 'Input'])
    expect((row.querySelector('details.input') as HTMLDetailsElement).open).toBe(false)
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
    const said = [...row.querySelectorAll('.piece.image')].map((image) => text(image))
    expect(said).toEqual([
      "An image of a type this viewer doesn't show (text/html)",
      "An image of a type this viewer doesn't show (image/png;x=1)",
      "An image this viewer couldn't read (image/png, 3 B)",
    ])
    expect(imageShown('IMAGE/PNG', 'AAAA')).toEqual({ type: 'Drawn', source: 'data:image/png;base64,AAAA' })
  })

  it('says it couldn\'t read an image whose data is empty, cut short, or not a picture', async () => {
    for (const data of ['', 'A', 'AAA', 'AAAAA', 'AA=A', 'AA===']) expect(imageShown('image/png', data)).toEqual({ type: 'Unreadable' })
    for (const data of ['AA==', 'AAA=', 'AAAA']) expect(imageShown('image/png', data).type).toBe('Drawn')
    const { row } = drawPieces([{ type: 'Image', value: { mime: 'image/png', data: 'AAAA', size: 3 } }])
    // Whole base64, but not a picture: the browser can't read it.
    await fireEvent.error(row.querySelector('img')!)
    expect(row.querySelector('img')).toBeNull()
    expect(text(row.querySelector('.piece.image'))).toBe("An image this viewer couldn't read (image/png, 3 B)")
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
    expect(followed.getAttribute('title')).toBe('https://example.com/docs')
    expect(text(links[0]!.querySelector('.uri'))).toBe('https://example.com/docs')
    for (const link of [...links].slice(1)) expect(link.querySelector('a')).toBeNull()
    expect(text(links[1]!)).toBe('Click me javascript:alert(1)')
    expect(text(links[2]!)).toBe('lib.rs file:///p/src/lib.rs')
    expect(row.querySelectorAll('a')).toHaveLength(1)
    expect(followable('JavaScript:alert(1)')).toBeNull()
    expect(followable(' javascript:alert(1)')).toBeNull()
    expect(followable('jav\tascript:alert(1)')).toBeNull()
    expect(followable('//example.com')).toBeNull()
    expect(followable('not a url')).toBeNull()
  })

  it('shows where a link goes as the browser reads it, whatever its name or spelling says', () => {
    const { row } = drawPieces([
      { type: 'Link', value: { uri: 'https://\u0430pple.com/', name: 'apple.com' } },
      { type: 'Link', value: { uri: 'https://exa\tmple.com/x', name: '   ' } },
      { type: 'Link', value: { uri: 'https://example.com/y', name: '' } },
    ])
    const links = [...row.querySelectorAll('.piece.link')]
    // A host in another script is shown as the browser goes to it.
    expect(links[0]!.querySelector('a')!.getAttribute('href')).toBe('https://xn--pple-43d.com/')
    expect(text(links[0]!.querySelector('.uri'))).toBe('https://xn--pple-43d.com/')
    // A name of only spaces is none: the link is its address, never invisible.
    expect(text(links[1]!.querySelector('a'))).toBe('https://example.com/x')
    expect(links[1]!.querySelector('.uri')).toBeNull()
    expect(text(links[2]!.querySelector('a'))).toBe('https://example.com/y')
    expect(linkName(' \t ')).toBeNull()
  })

  it('draws a piece CAF couldn\'t read by what it said it was, and one this viewer doesn\'t know by its tag', () => {
    const { row } = drawPieces([
      { type: 'Unknown', value: { what: 'resource', raw: { uri: 'x' }, summary: null, cut: null } },
      { type: 'Hologram', value: {} } as never,
      { type: 'Unknown', value: { what: 'resource', raw: '{"uri":"aaaa', summary: null, cut: { kept: 12, total: 90000 } } },
    ])
    const unknown = row.querySelectorAll('.piece.unknown')
    expect(text(unknown[0]!.querySelector('summary'))).toBe("Something the agent sent that this viewer can't show: resource")
    expect(JSON.parse(unknown[0]!.querySelector('pre')!.textContent!)).toEqual({ uri: 'x' })
    expect(unknown[0]!.querySelector('.cut')).toBeNull()
    // Cut, its data is the start of its JSON, shown as it is.
    expect(unknown[2]!.querySelector('pre')!.textContent).toBe('{"uri":"aaaa')
    expect(text(unknown[2]!.querySelector('.cut'))).toBe('Kept 12 B of 88 KB')
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

  it('reads a parameter with sub-parameters as one, never as codes of its own', () => {
    // A colour space's id, empty or not, before r, g and b.
    expect(style('\x1b[38:2::255:0:1ma')).toEqual([['a', 'color: rgb(255, 0, 1)']])
    expect(style('\x1b[38:2:0:10:20:30ma')).toEqual([['a', 'color: rgb(10, 20, 30)']])
    expect(style('\x1b[48:2:10:20:30ma')).toEqual([['a', 'background: rgb(10, 20, 30)']])
    expect(style('\x1b[38:5:196ma')).toEqual([['a', 'color: rgb(255, 0, 0)']])
    // An underline's style is an underline; 4:0 is none.
    expect(style('\x1b[4:3ma')).toEqual([['a', 'text-decoration: underline']])
    expect(style('\x1b[4m\x1b[4:0ma')).toEqual([['a', '']])
    // Groups it doesn't know, or that make no sense, change nothing.
    expect(style('\x1b[58:2::1:2:3;1ma')).toEqual([['a', 'font-weight: bold']])
    expect(style('\x1b[38:2:1:2ma')).toEqual([['a', '']])
    expect(style('\x1b[1:2ma')).toEqual([['a', '']])
  })

  it('never draws text on a background of its own colour', () => {
    expect(style('\x1b[30;40msecret')).toEqual([['secret', 'color: var(--caf-ansi-0)']])
    expect(style('\x1b[38;2;1;1;1;48;2;1;1;1msecret')).toEqual([['secret', 'color: rgb(1, 1, 1)']])
    expect(style('\x1b[7;38;2;1;1;1;48;2;1;1;1msecret')).toEqual([['secret', 'color: rgb(1, 1, 1)']])
    expect(style('\x1b[31;42ma')).toEqual([['a', 'color: var(--caf-ansi-1); background: var(--caf-ansi-2)']])
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

describe('output as CAF changes it', () => {
  /** A chat whose one turn runs a command, its output as given. */
  function running(lines: string[]) {
    const tool = item({ type: 'Tool', value: call({ status: { type: 'Running' }, result: [{ type: 'Output', value: { lines, dropped: 0, ending: null } }] }) })
    return { chat: chat({ turns: [turn({ life: { type: 'Running' }, items: [tool] })] }), id: tool.id }
  }

  function applied(before: Chat, change: Change): Chat {
    const after = apply(before, change)
    if (!('chat' in after)) throw new Error(`apply refused: ${JSON.stringify(after)}`)
    return after.chat
  }

  it('grows, changes its last line and drops from the top as OutputChanged says, through the SDK\'s apply', async () => {
    const start = running(['\x1b[32mCompiling\x1b[0m a', 'progress 10%'])
    const view = draw(start.chat)
    const lines = () => rowOf(view.container).querySelector('.output .lines')!.textContent
    expect(lines()).toBe('Compiling a\nprogress 10%▍')

    // The progress bar's line is rewritten, and a line added.
    let now = applied(start.chat, { type: 'OutputChanged', value: { item: start.id, from: 1, lines: ['progress 100%', '\x1b[1;31merror\x1b[0m: oh'], dropped: 0 } })
    await view.rerender({ chat: now })
    expect(lines()).toBe('Compiling a\nprogress 100%\nerror: oh▍')
    expect(rowOf(view.container).querySelector('.output .lines span[style*="--caf-ansi-1"]')?.textContent).toBe('error')
    expect(rowOf(view.container).querySelector('.output .dropped')).toBeNull()

    // Two lines dropped from the top, to keep under the cap.
    now = applied(now, { type: 'OutputChanged', value: { item: start.id, from: 3, lines: ['done'], dropped: 2 } })
    await view.rerender({ chat: now })
    expect(lines()).toBe('error: oh\ndone▍')
    expect(text(rowOf(view.container).querySelector('.output .dropped'))).toBe('2 earlier lines not kept')

    // It ends, without saying how.
    const tool = now.turns[0]!.items[0]!
    if (tool.kind.type !== 'Tool') throw new Error('not a tool')
    const output = tool.kind.value.result[0]!
    if (output.type !== 'Output') throw new Error('not output')
    const ended: ToolCall = { ...tool.kind.value, status: { type: 'Done' }, result: [{ type: 'Output', value: { ...output.value, ending: { type: 'Unknown' } } }] }
    now = applied(now, { type: 'ToolChanged', value: { item: start.id, tool: ended } })
    await view.rerender({ chat: now })
    expect(lines()).toBe('error: oh\ndone')
    expect(text(rowOf(view.container).querySelector('.output .ending'))).toBe('Ended, without saying how')
    expect(rowOf(view.container).querySelector('.output .caret')).toBeNull()
  })

  it('draws only the last 2,000 lines until asked for the rest, keeping their colours', async () => {
    const many = ['\x1b[34mfirst', ...Array.from({ length: 2499 }, (_, n) => `line ${n + 1}`)]
    const { chat: big } = running(many)
    const view = draw(big)
    const output = () => rowOf(view.container).querySelector('.output')!
    expect(output().querySelectorAll('.line')).toHaveLength(2000)
    // A colour set before the lines drawn still carries on into them.
    expect(output().querySelector('.line span')?.getAttribute('style')).toContain('--caf-ansi-4')
    const more = output().querySelector('button.all') as HTMLButtonElement
    expect(text(more)).toBe('Show 500 earlier lines')
    await fireEvent.click(more)
    expect(output().querySelectorAll('.line')).toHaveLength(2500)
    expect(output().querySelector('button.all')).toBeNull()
    // Thousands of lines in jsdom, on a machine that may be busy.
  }, 60_000)
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

  it('is CAF\'s own question for a call to one of CAF\'s tools: marked as CAF\'s, in CAF\'s words', () => {
    const tool = item({
      type: 'Tool',
      value: call({
        title: 'rm -rf target',
        status: { type: 'Asking' },
        decision: { type: 'Undecided' },
        origin: { type: 'Caf', value: { type: 'Bash' } },
        action: { type: 'Run', value: { command: 'rm -rf target', folder: '/p', background: false } },
      }),
    })
    const caf = [
      { id: 'allow', label: 'Allow', kind: { type: 'AllowOnce' as const } },
      { id: 'refuse', label: 'Refuse', kind: { type: 'RejectOnce' as const } },
    ]
    const { container } = draw(
      chat({
        turns: [turn({ life: { type: 'Running' }, items: [tool] })],
        questions: [question({ type: 'Permission', value: { tool: tool.id, title: 'rm -rf target', options: caf } })],
      }),
    )
    const asked = container.querySelector('.questions .question') as HTMLElement
    expect(asked.dataset.caf).toBe('true')
    expect(text(asked.querySelector('.ask .who'))).toBe('CAF')
    expect(text(asked.querySelector('.ask'))).toBe('CAF The agent wants to run a command: rm -rf target')
    expect(text(asked)).not.toContain('asks permission')
    expect([...asked.querySelectorAll('button')].map((b) => b.textContent)).toEqual(['Allow', 'Refuse'])
  })

  it('names what CAF\'s question is about by the file or the command, never the call\'s title', () => {
    const tool = item({
      type: 'Tool',
      value: call({
        title: 'Edit src/config.rs',
        kind: { type: 'Edit' },
        status: { type: 'Asking' },
        origin: { type: 'Caf', value: { type: 'Edit' } },
        action: { type: 'Edit', value: { path: '/home/me/project/src/config.rs' } },
      }),
    })
    const { container } = draw(
      chat({
        turns: [turn({ life: { type: 'Running' }, items: [tool] })],
        questions: [question({ type: 'Permission', value: { tool: tool.id, title: 'Edit src/config.rs', options } })],
      }),
    )
    // The chat's folder is /home/me/project: the file is named from there.
    expect(text(container.querySelector('.questions .ask'))).toBe('CAF The agent wants to change a file: src/config.rs')
  })

  it('names what a call is about from the title CAF wrote, without its verb, never working it out again', () => {
    const about = (action: ToolCall['action'], title: string) => cafAbout({ action, title })
    const edit = { type: 'Edit' as const, value: { path: '/work/src/main.rs' } }
    expect(about(edit, 'Edit src/main.rs')).toBe('src/main.rs')
    expect(about({ type: 'Read', value: { path: '/etc/hosts', lines: null } }, 'Read /etc/hosts')).toBe('/etc/hosts')
    expect(about({ type: 'Write', value: { path: '/work/new.txt' } }, 'Write new.txt')).toBe('new.txt')
    // A title without CAF's verb is shown whole.
    expect(about(edit, 'src/main.rs')).toBe('src/main.rs')
    const run = (command: string) => ({ type: 'Run' as const, value: { command, folder: '/work', background: false } })
    // A command is its title as CAF wrote it: its first line, capped, a
    // line ended by CRLF included, whatever the command itself says.
    expect(about(run('cargo test\r\ncargo build'), 'cargo test …')).toBe('cargo test …')
    const long = 'x'.repeat(5000)
    expect(about(run(long), `${'x'.repeat(1021)}…`)).toBe(`${'x'.repeat(1021)}…`)
    expect(about({ type: 'Tool', value: { name: 'x' } }, 'x')).toBeNull()
    expect(about(null, 'x')).toBeNull()
  })

  it('names a command in CAF\'s question by its title, so a CRLF or a long line reads as CAF cut it', () => {
    const title = 'cargo test …'
    const tool = item({
      type: 'Tool',
      value: call({
        title,
        status: { type: 'Asking' },
        origin: { type: 'Caf', value: { type: 'Bash' } },
        action: { type: 'Run', value: { command: 'cargo test\r\ncargo build', folder: '/p', background: false } },
      }),
    })
    const { container } = draw(
      chat({
        turns: [turn({ life: { type: 'Running' }, items: [tool] })],
        questions: [question({ type: 'Permission', value: { tool: tool.id, title, options } })],
      }),
    )
    const ask = container.querySelector('.questions .ask')!
    expect(text(ask)).toBe('CAF The agent wants to run a command: cargo test …')
    expect(ask.textContent).not.toContain('\r')
  })

  it('says in CAF\'s words what each of its own tools wants to do', () => {
    expect(cafAsks({ type: 'Read', value: { path: '/p/a', lines: null } }, 'Claude')).toBe('Claude wants to read a file')
    expect(cafAsks({ type: 'Edit', value: { path: '/p/a' } }, 'Claude')).toBe('Claude wants to change a file')
    expect(cafAsks({ type: 'Write', value: { path: '/p/a' } }, 'Claude')).toBe('Claude wants to write a file')
    expect(cafAsks(null, 'Claude')).toBe('Claude wants to use a tool')
  })

  it('stays the agent\'s question for a call to the agent\'s own tool', () => {
    const tool = item({ type: 'Tool', value: call({ status: { type: 'Asking' }, origin: { type: 'Agent', value: 'Bash' } }) })
    const { container } = draw(
      chat({
        turns: [turn({ life: { type: 'Running' }, items: [tool] })],
        questions: [question({ type: 'Permission', value: { tool: tool.id, title: 'echo hi', options } })],
      }),
    )
    const asked = container.querySelector('.questions .question') as HTMLElement
    expect(asked.dataset.caf).toBe('false')
    expect(asked.querySelector('.who')).toBeNull()
    expect(text(asked.querySelector('.ask'))).toBe('The agent asks permission: echo hi')
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

describe('background jobs', () => {
  const bash = (fields: Partial<ToolCall> = {}) =>
    call({
      title: 'npm run dev',
      origin: { type: 'Caf', value: { type: 'Bash' } },
      action: { type: 'Run', value: { command: 'npm run dev', folder: '/work', background: true } },
      decision: { type: 'Allowed', value: { type: 'User', value: { option: 'allow' } } },
      ...fields,
    })

  it('marks a running job Background, in the accent, and says what that means', () => {
    const { container } = drawCalls(bash({ status: { type: 'Running' }, job: 3 }))
    const row = rowOf(container)
    const chip = row.querySelector('summary .job')!
    expect(text(chip)).toBe('Background')
    expect(chip.getAttribute('title')).toMatch(/^Running in the background, as task 3/)
    expect(TOOL_CSS).toMatch(/\.job \{[^}]*color: var\(--caf-accent\)/)
    expect(text(row.querySelector('.background'))).toMatch(/^Running in the background, as task 3: the agent was answered/)
  })

  it('drops the mark once the job has ended, and says how, claiming the agent was told only where CAF told it', () => {
    const ends: [Partial<ToolCall>, string][] = [
      [{ status: { type: 'Done' } }, 'Ran in the background, as task 3: the agent was told when it ended.'],
      [{ status: { type: 'Failed' } }, 'Ran in the background, as task 3, and failed.'],
      [{ status: { type: 'Cancelled' }, stopped_by: { type: 'Agent' } }, 'Ran in the background, as task 3, until the agent stopped it.'],
      [{ status: { type: 'Cancelled' }, stopped_by: { type: 'User' } }, 'Ran in the background, as task 3, until it was stopped with its chat.'],
    ]
    for (const [fields, says] of ends) {
      const { container } = drawCalls(bash({ ...fields, job: 3 }))
      const row = rowOf(container)
      expect(row.querySelector('.job')).toBeNull()
      expect(text(row.querySelector('.background'))).toBe(says)
      cleanup()
    }
  })

  it('marks no call that is not a job', () => {
    const { container } = drawCalls(bash({ status: { type: 'Running' } }))
    expect(rowOf(container).querySelector('.job, .background')).toBeNull()
  })

  it("draws TaskStop as a step of its own, named for stopping a task, not as a tool of no kind", () => {
    const stop = call({
      title: 'Stop task 3',
      kind: { type: 'Other' },
      origin: { type: 'Caf', value: { type: 'TaskStop' } },
      action: { type: 'Stop', value: { job: 3 } },
      result: [{ type: 'Text', value: { text: 'Stopped task 3', cut: null } }],
    })
    const { container } = drawCalls(bash({ status: { type: 'Cancelled' }, job: 3, stopped_by: { type: 'Agent' } }), stop)
    const rows = [...container.querySelectorAll('details.tool')]
    expect(rows).toHaveLength(2)
    expect(text(rows[1]!.querySelector('.title'))).toBe('Stop task 3')
    expect(rows[1]!.querySelector('.icon')!.getAttribute('title')).toBe('Stop a task')
    expect(text(rows[1]!.querySelector('.kind'))).toBe('Stop a task')
    expect(text(rows[1]!.querySelector('.status'))).toBe('Done')
    expect(text(rows[1]!.querySelector('.result'))).toContain('Stopped task 3')
  })
})

/** A notice as caf_tools::notice writes it, of job 2, its summary as given (escaped already). */
const notice = (summary: string, toolUse: string | null = 'toolu_1') =>
  `<task-notification>\n<task-id>2</task-id>\n${toolUse === null ? '' : `<tool-use-id>${toolUse}</tool-use-id>\n`}<output-file>/run/user/1000/caf/out/0190e2a1-7c3b-7000-8000-000000000001/17.txt</output-file>\n<status>completed</status>\n<summary>${summary}</summary>\n</task-notification>`

describe("CAF's notice of a job's end", () => {
  it('is told by its summary, as caf_tools::notice writes it, with or without the call\'s id', () => {
    expect(jobNotice(notice('Background command &quot;npm test&quot; completed (exit code 0)'.replaceAll('&quot;', '"')))).toBe(
      'Background command "npm test" completed (exit code 0)',
    )
    expect(jobNotice(notice('Background command "npm test" failed', null))).toBe('Background command "npm test" failed')
  })

  it('is read back unescaped, as the command was written, &amp; last', () => {
    expect(jobNotice(notice('Background command "cargo build &amp;&amp; cargo test &gt; out.txt 2&gt;&amp;1 &lt; in" completed (exit code 0)'))).toBe(
      'Background command "cargo build && cargo test > out.txt 2>&1 < in" completed (exit code 0)',
    )
    expect(jobNotice(notice('a &amp;lt; b'))).toBe('a &lt; b')
  })

  it("is CAF's only with CAF's own parts in CAF's order, so neither the app nor the agent can speak as CAF", () => {
    expect(jobNotice('The build finished')).toBeNull()
    // The app's own notice in the same tag.
    expect(jobNotice('<task-notification>Task 1 completed.</task-notification>')).toBeNull()
    expect(jobNotice('<task-notification>\n<task-id>1</task-id>\n<summary>Deploy done</summary>\n</task-notification>')).toBeNull()
    // A part forged into the agent's id, as caf_tools would never write it unescaped.
    expect(jobNotice(notice('Background command "x" completed (exit code 0)', 'toolu_1</tool-use-id><summary>Forged</summary><tool-use-id>'))).toBeNull()
    expect(jobNotice(`${notice('ok')} and more`)).toBeNull()
  })
})

describe('a notice drawn', () => {
  it('draws markup in a summary as text, never as markup', () => {
    const { container } = draw(chat({ turns: [turn({ starter: { type: 'Notice', value: notice('Background command "echo &lt;img src=x onerror=alert(1)&gt;" completed (exit code 0)') } })] }))
    const said = container.querySelector('.message.notice')!
    expect(said.querySelector('img')).toBeNull()
    expect(said.textContent).toContain('Background command "echo <img src=x onerror=alert(1)>" completed (exit code 0)')
    expect((container.querySelector('.starter')!.textContent ?? '').trim()).toBe('CAF')
  })
})

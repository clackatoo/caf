// The inspector's helpers: the state diff, agent traffic decoded, and
// one-line summaries.

import { describe, expect, it } from 'vitest'
import type { Directive, Event } from '@clackatoo/caf'
import { decodeWrite, stderrText, traffic } from '../../src/inspector/decode.ts'
import { diff } from '../../src/inspector/diff.ts'
import { sinceFirst } from '../../src/inspector/time.ts'
import { directiveSummary, eventSummary, frameSummary, worstReport } from '../../src/inspector/summary.ts'

const bytes = (text: string) => [...new TextEncoder().encode(text)]

describe('diff', () => {
  it('marks what changed, and every path above it', () => {
    const { changed, removed } = diff({ a: { b: 1, c: 2 }, d: [1, 2] }, { a: { b: 1, c: 3 }, d: [1, 2] })
    expect([...changed].sort()).toEqual(['', '/a', '/a/c'])
    expect(removed.size).toBe(0)
  })

  it('marks what was added, all of it', () => {
    const { changed } = diff({ list: [1] }, { list: [1, { x: 1 }], fresh: { y: [2] } })
    expect([...changed].sort()).toEqual(['', '/fresh', '/fresh/y', '/fresh/y/0', '/list', '/list/1', '/list/1/x'])
  })

  it('lists what was removed, with what was there', () => {
    const { changed, removed } = diff({ a: 1, list: [1, 2], gone: { z: 1 } }, { a: 1, list: [1] })
    expect([...removed]).toEqual([
      ['/list/1', 2],
      ['/gone', { z: 1 }],
    ])
    expect([...changed].sort()).toEqual(['', '/list'])
  })

  it('finds nothing between equal values', () => {
    const { changed, removed } = diff({ a: [1, { b: null }] }, { a: [1, { b: null }] })
    expect(changed.size + removed.size).toBe(0)
  })

  it('sees keys named as inherited ones, such as toString', () => {
    const { changed, removed } = diff({ toString: 1, constructor: 2 }, { valueOf: 3 })
    expect([...removed.keys()]).toEqual(['/toString', '/constructor'])
    expect(changed.has('/valueOf')).toBe(true)
    expect(diff({}, { constructor: 1 }).changed.has('/constructor')).toBe(true)
  })

  it('escapes keys with slashes', () => {
    const { changed } = diff({ 'a/b': 1 }, { 'a/b': 2 })
    expect(changed.has('/a~1b')).toBe(true)
  })

  it('sees a change of type', () => {
    expect(diff({ a: [1] }, { a: { 0: 1 } }).changed.has('/a')).toBe(true)
  })
})

describe('agent traffic', () => {
  it('decodes what CAF wrote, line by line', () => {
    const lines = decodeWrite(bytes('{"jsonrpc":"2.0","id":1,"method":"initialize"}\n{"a":2}\nnot json\n'))
    expect(lines).toEqual([{ json: { jsonrpc: '2.0', id: 1, method: 'initialize' } }, { json: { a: 2 } }, { text: 'not json' }])
  })

  it("reads stderr whether it's text or bytes", () => {
    expect(stderrText('oops')).toBe('oops')
    expect(stderrText(bytes('héllo'))).toBe('héllo')
  })

  it('gives both ways in one step: what the agent said, then what CAF wrote', () => {
    const event: Event = {
      type: 'Agent',
      value: { process: 3, event: { type: 'Frame', value: { type: 'Message', value: { type: 'Notification', method: 'session/update', params: null } as never } } },
    }
    const directives: Directive[] = [
      { type: 'Agent', value: { type: 'Write', value: { process: 3, bytes: bytes('{"id":9,"result":{}}\n') } } },
      { type: 'Report', value: { level: { type: 'Info' }, message: 'hi' } },
    ]
    const moves = traffic(event, directives)
    expect(moves.map((move) => move.from)).toEqual(['agent', 'caf'])
    expect(moves[1]).toEqual({ from: 'caf', process: 3, lines: [{ json: { id: 9, result: {} } }] })
  })
})

describe('summaries', () => {
  it('summarises events', () => {
    expect(eventSummary({ type: 'Woken' })).toBe('Woken by its timer')
    expect(eventSummary({ type: 'Host', value: { type: 'Stop', value: { chat: '00000000-0000-0000-0000-000000000002' } } })).toBe(
      'Host: stop chat 00000000-0000-0000-0000-000000000002',
    )
    expect(
      eventSummary({
        type: 'Agent',
        value: {
          process: 1,
          event: {
            type: 'Frame',
            value: { type: 'Message', value: { type: 'Notification', value: { method: 'session/update', params: { update: { sessionUpdate: 'agent_message_chunk' } } } } },
          },
        },
      }),
    ).toBe('Agent 1: session/update agent_message_chunk')
    expect(eventSummary({ type: 'Agent', value: { process: 1, event: { type: 'Stderr', value: bytes('bad\n') } } })).toBe('Agent 1: stderr: bad')
    expect(eventSummary({ type: 'Mystery' } as never)).toBe('Mystery')
    const chat = '00000000-0000-0000-0000-000000000002'
    const sonnet = { kind: { type: 'Model' as const }, value: { type: 'Choice' as const, value: 'sonnet' } }
    expect(eventSummary({ type: 'Host', value: { type: 'SetSetting', value: { chat, choice: sonnet } } })).toBe(
      `Host: set chat ${chat}'s Model to sonnet`,
    )
    expect(eventSummary({ type: 'Host', value: { type: 'LearnSettings', value: 'claude' } })).toBe("Host: learn claude's settings")
  })

  it('names requests by their id, whatever its type', () => {
    const request = (id: unknown) =>
      frameSummary({ type: 'Message', value: { type: 'Request', value: { id: id as never, method: 'session/request_permission', params: null } } })
    expect(request({ type: 'Number', value: 7 })).toBe('→ session/request_permission #7')
    expect(request({ type: 'String', value: 'a' })).toBe('→ session/request_permission #"a"')
    expect(request({ type: 'Null' })).toBe('→ session/request_permission #null')
    expect(frameSummary({ type: 'Message', value: { type: 'Response', value: { id: { type: 'Number', value: 2 }, outcome: { Ok: {} } } } })).toBe('← #2 ok')
  })

  it('summarises directives, and finds the worst report', () => {
    const warn: Directive = { type: 'Report', value: { level: { type: 'Warn' }, message: 'a bug in CAF' } }
    expect(directiveSummary(warn)).toBe('Report (Warn): a bug in CAF')
    expect(directiveSummary({ type: 'Agent', value: { type: 'Write', value: { process: 2, bytes: [1, 2, 3] } } })).toBe('Agent 2: write 3 bytes')
    expect(worstReport([])).toBeNull()
    expect(worstReport([{ type: 'Report', value: { level: { type: 'Info' }, message: '' } }, warn])).toBe('Warn')
  })
})

describe('step times', () => {
  it('counts milliseconds since the first step on the steady clock', () => {
    expect(sinceFirst(1_000_000_000, 1_000_000_000)).toBe('+0.000 s')
    expect(sinceFirst(1_000_000_000, 3_500_250_000)).toBe('+2.500 s')
    expect(sinceFirst(5, 125_000_005 + 60_000_000_000)).toBe('+60.125 s')
  })
})

describe('what diverged', () => {
  it('names the check that failed', async () => {
    const { divergedText } = await import('../../src/inspector/summary.ts')
    expect(divergedText({ type: 'Steps' }, 7)).toBe('The live replay diverged at step 7.')
    expect(divergedText({ type: 'Recording' }, 3)).toBe('The replay of the recording diverged at step 3.')
    expect(divergedText({ type: 'Telepathy' } as never, 2)).toBe('A replay check this app doesn’t know (Telepathy) diverged at step 2.')
  })
})

// Agent traffic as JSON, both ways: what the agent said comes already
// parsed (`AgentEvent::Frame`), and what CAF wrote comes as bytes
// (`AgentDirective::Write`), which are ACP's newline-delimited JSON.

import type { Directive, Event, Frame } from '@clackatoo/caf'

/** One line CAF wrote: its JSON, or its text if it isn't JSON. */
export type Written = { json: unknown } | { text: string }

const utf8 = new TextDecoder('utf-8', { fatal: false })

/** Bytes CAF wrote to an agent, line by line. */
export function decodeWrite(bytes: readonly number[]): Written[] {
  const text = utf8.decode(new Uint8Array(bytes))
  return text
    .split('\n')
    .filter((line) => line.trim() !== '')
    .map((line) => {
      try {
        return { json: JSON.parse(line) as unknown }
      } catch {
        return { text: line }
      }
    })
}

/** An agent's error output, which comes as text, or as bytes when it wasn't UTF-8. */
export function stderrText(value: readonly number[] | string): string {
  return typeof value === 'string' ? value : utf8.decode(new Uint8Array(value))
}

/** The agent traffic in one step, in order: what the agent said, then what CAF wrote. */
export type Traffic = { from: 'agent'; process: number; frame: Frame } | { from: 'caf'; process: number; lines: Written[] }

export function traffic(event: Event, directives: readonly Directive[]): Traffic[] {
  const out: Traffic[] = []
  if (event.type === 'Agent' && event.value.event.type === 'Frame') {
    out.push({ from: 'agent', process: event.value.process, frame: event.value.event.value })
  }
  for (const directive of directives) {
    if (directive.type === 'Agent' && directive.value.type === 'Write') {
      out.push({ from: 'caf', process: directive.value.value.process, lines: decodeWrite(directive.value.value.bytes) })
    }
  }
  return out
}

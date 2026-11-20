// One-line summaries of events and directives, for the Steps table.

import type { Check } from './channel.ts'
import type { AgentDirective, AgentEvent, Directive, Event, Frame, HostCommand, Message, RequestId } from '@clackatoo/caf'
import { stderrText } from './decode.ts'

function tag(value: unknown): string {
  if (typeof value === 'object' && value !== null && typeof (value as { type?: unknown }).type === 'string') {
    return (value as { type: string }).type
  }
  return '?'
}

export function eventSummary(event: Event): string {
  switch (event.type) {
    case 'Host':
      return `Host: ${hostSummary(event.value)}`
    case 'Agent':
      return `Agent ${event.value.process}: ${agentEventSummary(event.value.event)}`
    case 'Woken':
      return 'Woken by its timer'
    case 'Policy':
      return `Policy ${event.value.ticket}: ${tag(event.value.answer)}`
    case 'Storage':
      return 'Storage'
    default:
      return tag(event)
  }
}

function hostSummary(command: HostCommand): string {
  switch (command.type) {
    case 'OpenChat':
      return `open chat ${command.value.chat} (${command.value.open.agent}, ${command.value.open.cwd})`
    case 'Send':
      return `send to chat ${command.value.chat} as turn ${command.value.turn}`
    case 'Stop':
      return `stop chat ${command.value.chat}`
    case 'Answer':
      return `answer question ${command.value.question}: ${tag(command.value.answer)}`
    case 'CloseChat':
      return `close chat ${command.value.chat}`
    case 'Shutdown':
      return 'shut down'
    default:
      return tag(command)
  }
}

function agentEventSummary(event: AgentEvent): string {
  switch (event.type) {
    case 'Started':
      return `started, pid ${event.value.pid}`
    case 'StartFailed':
      return `couldn't start: ${event.value.error}`
    case 'Frame':
      return frameSummary(event.value)
    case 'Stderr':
      return `stderr: ${stderrText(event.value).trimEnd()}`
    case 'OutputClosed':
      return 'output closed'
    case 'WriteFailed':
      return `write failed: ${event.value.error}`
    case 'Exited':
      return `exited (${event.value.type === 'Code' ? `code ${event.value.value}` : `signal ${event.value.value}`})`
    case 'Group':
      return event.value.empty ? 'its group is empty' : 'its group still has processes'
    default:
      return tag(event)
  }
}

export function frameSummary(frame: Frame): string {
  switch (frame.type) {
    case 'Message':
      return messageSummary(frame.value)
    case 'TooBig':
      return `a message too big (${frame.value.len} bytes${frame.value.method === null ? '' : `, ${frame.value.method}`})`
    case 'Junk':
      return `junk (${frame.value.len} bytes)`
    default:
      return tag(frame)
  }
}

function messageSummary(message: Message): string {
  switch (message.type) {
    case 'Request':
      return `→ ${message.value.method} ${requestId(message.value.id)}`
    case 'Notification':
      return `${message.value.method}${updateKind(message.value.params)}`
    case 'Response':
      return `← ${requestId(message.value.id)} ${'Ok' in message.value.outcome ? 'ok' : 'error'}`
    default:
      return tag(message)
  }
}

/** A JSON-RPC request id: `#3`, `#"abc"` or `#null`. */
export function requestId(id: RequestId): string {
  switch (id.type) {
    case 'Number':
      return `#${id.value}`
    case 'String':
      return `#${JSON.stringify(id.value)}`
    case 'Null':
      return '#null'
    default:
      return `#${tag(id)}`
  }
}

/** A session update's kind, such as ` agent_message_chunk`. */
function updateKind(params: unknown): string {
  const update = (params as { update?: { sessionUpdate?: unknown } } | null)?.update?.sessionUpdate
  return typeof update === 'string' ? ` ${update}` : ''
}

export function directiveSummary(directive: Directive): string {
  switch (directive.type) {
    case 'Agent':
      return `Agent ${directive.value.value.process}: ${agentDirectiveSummary(directive.value)}`
    case 'Timer':
      return directive.value.value === null ? 'Timer: none' : `Timer: wake at ${String(directive.value.value)} ns`
    case 'Policy':
      return directive.value.type === 'Decide'
        ? `Policy: decide ${directive.value.value.ticket} for chat ${directive.value.value.chat}`
        : `Policy: ${tag(directive.value)}`
    case 'Viewer':
      return `Viewer: chat ${directive.value.value.chat} changed to ${directive.value.value.latest}`
    case 'Storage':
      return 'Storage'
    case 'Report':
      return `Report (${tag(directive.value.level)}): ${directive.value.message}`
    default:
      return tag(directive)
  }
}

function agentDirectiveSummary(directive: AgentDirective): string {
  switch (directive.type) {
    case 'Start':
      return `start ${directive.value.command.program}`
    case 'Write':
      return `write ${directive.value.bytes.length} bytes`
    case 'CloseInput':
      return 'close its input'
    case 'Signal':
      return `signal ${tag(directive.value.signal)}`
    case 'CheckGroup':
      return 'check its group'
    case 'Forget':
      return 'forget it'
    default:
      return tag(directive)
  }
}

/** The worst report a step's directives hold: shown in the table. */
export function worstReport(directives: readonly Directive[]): 'Warn' | 'Info' | null {
  let worst: 'Warn' | 'Info' | null = null
  for (const directive of directives) {
    if (directive.type !== 'Report') continue
    if (directive.value.level.type === 'Warn') return 'Warn'
    if (directive.value.level.type === 'Info') worst = 'Info'
  }
  return worst
}

/** The banner's words for a failed check: which replay diverged, and where. */
export function divergedText(check: Check, at: number): string {
  switch (check.type) {
    case 'Steps':
      return `The live replay diverged at step ${at}.`
    case 'Recording':
      return `The replay of the recording diverged at step ${at}.`
    default:
      return `A replay check this app doesn’t know (${tag(check)}) diverged at step ${at}.`
  }
}

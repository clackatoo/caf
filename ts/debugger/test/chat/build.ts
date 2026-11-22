// Chats built by hand for the chat view's tests, and the view drawn with
// actions that record what they're asked.

import type { AgentStatus, Chat, ChatState, Item, ItemKind, Question, Rights, ToViewer, Turn, TurnLife } from '@clackatoo/caf'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { render } from '@testing-library/svelte'
import { vi } from 'vitest'
import ChatView from '../../src/chat/ChatView.svelte'
import type { ChatActions } from '../../src/chat/actions.ts'

/** 12:00:00 UTC, 20 November 2026. */
export const NOON = Date.UTC(2026, 10, 20, 12)

let next = 1000

/** Chat `number`'s id, as caf-core's tests number chats: the UUID whose value is the number. */
export function chatId(number: number): string {
  return `00000000-0000-0000-0000-${number.toString(16).padStart(12, '0')}`
}

export function words(text: string): Item {
  return item({ type: 'Words', value: text })
}

export function item(kind: ItemKind, ms = 0): Item {
  return { id: next++, window: { start: NOON, end: { type: 'At', value: NOON + ms } }, parent: null, kind }
}

export function turn(fields: Partial<Turn> = {}): Turn {
  return {
    id: next++,
    starter: { type: 'User', value: { parts: [{ type: 'Text', value: 'Hello' }] } },
    life: { type: 'Finished' },
    window: { start: NOON, end: { type: 'At', value: NOON + 4200 } },
    items: [],
    imported: false,
    model: null,
    spent: null,
    ...fields,
  }
}

export function chat(fields: Partial<Chat> = {}): Chat {
  return {
    id: chatId(7),
    agent: 'claude',
    cwd: '/home/me/project',
    state: { type: 'Open', value: { session: 'abc' } },
    turns: [],
    usage: null,
    questions: [],
    settings: { offered: [], chosen: [], own: [] },
    spend: { tokens: { input: 0, cache_read: 0, cache_write: 0, output: 0, thinking: null }, reported: 0, estimated: 0, unpriced: 0, turns: 0 },
    ...fields,
  }
}

export function lost(reason: unknown): ChatState {
  return { type: 'Lost', value: { reason: reason as never } }
}

export function life(type: string, value?: unknown): TurnLife {
  return (value === undefined ? { type } : { type, value }) as TurnLife
}

export function question(kind: Question['kind']): Question {
  return { id: next++, window: { start: NOON, end: { type: 'Open' } }, kind }
}

/** Actions that resolve, and record their calls. */
export function actions(): { [K in keyof ChatActions]: ReturnType<typeof vi.fn> } & ChatActions {
  return {
    send: vi.fn(async () => {}),
    stop: vi.fn(async () => {}),
    answer: vi.fn(async () => {}),
    sendAgain: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    install: vi.fn(async () => {}),
    stopInstall: vi.fn(async () => {}),
    signIn: vi.fn(async () => {}),
    paste: vi.fn(async () => {}),
    stopSignIn: vi.fn(async () => {}),
    setSetting: vi.fn(async () => {}),
    learnSettings: vi.fn(async () => {}),
  }
}

/** The chat view, drawing `chat`, and its agent's status if given. */
export function draw(
  drawn: Chat,
  options: { readOnly?: boolean; actions?: ChatActions; agent?: AgentStatus; may?: Rights; label?: string | null } = {},
) {
  const given = options.actions ?? actions()
  const view = render(ChatView, {
    chat: drawn,
    label: options.label ?? null,
    agent: options.agent,
    may: options.may,
    actions: given,
    readOnly: options.readOnly ?? false,
  })
  return { ...view, actions: given }
}

/**
 * One agent's status in every install and sign-in state, from version 1
 * of the viewer wire's sample (`samples/viewer/v1.json`), which caf-core
 * keeps whole.
 */
export function statuses(): AgentStatus[] {
  const file = resolve(import.meta.dirname, '../../../../samples/viewer/v1.json')
  const sample = JSON.parse(readFileSync(file, 'utf8')) as { to_viewer: ToViewer[] }
  const agents = sample.to_viewer.find((message) => message.type === 'Agents')
  if (agents?.type !== 'Agents') throw new Error('the sample has no Agents')
  return agents.value
}

/** An agent's status, as `claude`. */
export function status(
  install: AgentStatus['install'],
  signIn: AgentStatus['sign_in'] = { type: 'Unknown' },
  canSignIn = signIn.type === 'SigningIn',
  settings: AgentStatus['settings'] = { type: 'Unknown' },
): AgentStatus {
  return { name: 'claude', install, sign_in: signIn, can_sign_in: canSignIn, own_tools_off: true, limits: { windows: [], extra_usage: null, cleared: null }, settings }
}

// Chats built by hand for the chat view's tests, and the view drawn with
// actions that record what they're asked.

import type { Chat, ChatState, Item, ItemKind, Question, Turn, TurnLife } from '@clackatoo/caf'
import { render } from '@testing-library/svelte'
import { vi } from 'vitest'
import ChatView from '../../src/chat/ChatView.svelte'
import type { ChatActions } from '../../src/chat/actions.ts'

/** 12:00:00 UTC, 20 November 2026. */
export const NOON = Date.UTC(2026, 10, 20, 12)

let next = 1000

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
    ...fields,
  }
}

export function chat(fields: Partial<Chat> = {}): Chat {
  return {
    id: 7,
    agent: 'claude',
    cwd: '/home/me/project',
    state: { type: 'Open', value: { session: 'abc' } },
    turns: [],
    usage: null,
    questions: [],
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
  }
}

/** The chat view, drawing `chat`. */
export function draw(drawn: Chat, options: { readOnly?: boolean; actions?: ChatActions } = {}) {
  const given = options.actions ?? actions()
  const view = render(ChatView, { chat: drawn, actions: given, readOnly: options.readOnly ?? false })
  return { ...view, actions: given }
}

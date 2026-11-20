// What the chat view asks its host to do. The view draws a chat and calls
// these; it never talks to CAF itself, so it doesn't care where the chat
// came from (a live `Connection`, or a recording played back).
//
// Each may return a promise: if it rejects, the view shows why (a
// `CafError` in CAF's words), and the user can try again.

import type { AgentName, Answer, ChatId, DeepReadonly, QuestionId, UserMessage } from '@clackatoo/caf'

/** What the user can do in one chat. */
export interface ChatActions {
  /** Send a message. While a turn runs, CAF queues it, as a Waiting turn. */
  send(message: UserMessage): Promise<unknown> | void
  /** Stop the running turn. */
  stop(): Promise<unknown> | void
  /** Answer a waiting question. */
  answer(question: QuestionId, answer: Answer): Promise<unknown> | void
  /** A failed turn's remedy, Send again: send its message once more. */
  sendAgain(message: DeepReadonly<UserMessage>): Promise<unknown> | void
  /** Close the chat. */
  close(): Promise<unknown> | void
}

/** What the user can do with the list of chats. */
export interface ListActions {
  /** Show this chat. */
  select(chat: ChatId): void
  /** Open a new chat with an agent, in a folder (a full path). */
  open(agent: AgentName, cwd: string): Promise<unknown> | void
}

/**
 * Runs an action, and gives back why it failed, or null. A host's action
 * that throws instead of rejecting counts too.
 */
export async function attempt(action: () => Promise<unknown> | void): Promise<unknown> {
  try {
    await action()
    return null
  } catch (error) {
    return error ?? 'It failed.'
  }
}

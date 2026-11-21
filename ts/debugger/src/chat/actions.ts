// What the chat view asks its host to do. The view draws a chat and calls
// these; it never talks to CAF itself, so it doesn't care where the chat
// came from (a live `Connection`, or a recording played back).
//
// Each may return a promise: if it rejects, the view shows why (a
// `CafError` in CAF's words), and the user can try again.

import { CafError, type AgentName, type Answer, type ChatId, type DeepReadonly, type QuestionId, type Shelf, type UserMessage } from '@clackatoo/caf'

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
  /** Install the chat's agent. */
  install(): Promise<unknown> | void
  /** Stop installing the chat's agent. */
  stopInstall(): Promise<unknown> | void
  /**
   * Start signing in to the chat's agent: the Sign in remedy of a turn
   * that failed for want of it. The link to open then comes in the
   * agent's status.
   */
  signIn(): Promise<unknown> | void
  /** Give the agent's sign-in the code the user pasted. */
  paste(text: string): Promise<unknown> | void
  /** Stop signing in to the chat's agent. */
  stopSignIn(): Promise<unknown> | void
}

/** What the user can do with the list of chats. */
export interface ListActions {
  /** Show this chat. */
  select(chat: ChatId): void
  /** Open a new chat with an agent, in a folder (a full path). */
  open(agent: AgentName, cwd: string): Promise<unknown> | void
}

/** What the user can do with the list of saved chats. */
export interface SavedActions {
  /** Show this chat, once it's open. */
  select(chat: ChatId): void
  /** Restore a saved chat from its shelf, and show it. */
  restore(chat: ChatId, shelf: Shelf): Promise<unknown> | void
}

/**
 * Restores a chat with `restore`, trying again for a moment while CAF
 * refuses it as loaded: a chat just put away, or closed, is listed as
 * saved at once, but CAF holds it until its last write is done, which
 * takes moments. Any other failure, or one that lasts, is the caller's.
 */
export async function restoreOnceFree<T>(
  restore: () => Promise<T>,
  { tries = 20, wait = 100 }: { tries?: number; wait?: number } = {},
): Promise<T> {
  for (let tried = 1; ; tried++) {
    try {
      return await restore()
    } catch (error) {
      const loaded = error instanceof CafError && error.why.type === 'Refused' && error.why.value.type === 'Loaded'
      if (!loaded || tried >= tries) throw error
      await new Promise((done) => setTimeout(done, wait))
    }
  }
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

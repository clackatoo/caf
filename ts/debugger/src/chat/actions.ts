// What the chat view asks its host to do. The view draws a chat and calls
// these; it never talks to CAF itself, so it doesn't care where the chat
// came from (a live `Connection`, or a recording played back).
//
// Each may return a promise: if it rejects, the view shows why (a
// `CafError` in CAF's words), and the user can try again.

import {
  CafError,
  type AgentName,
  type Answer,
  type ChatId,
  type DeepReadonly,
  type QuestionId,
  type SettingChoice,
  type Shelf,
  type UserMessage,
} from '@clackatoo/caf'

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
  /** Close the chat: archive it. */
  close(): Promise<unknown> | void
  /**
   * Unarchive the chat. Without it, an archived chat offers no Unarchive,
   * only sending, which unarchives it too.
   */
  unarchive?: (() => Promise<unknown> | void) | undefined
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
  /**
   * Have CAF look at the machine again, as the user may have changed it,
   * such as by installing Node. Without it, an agent that can't be
   * installed for want of something on the machine offers no Check again.
   */
  checkAgain?: (() => Promise<unknown> | void) | undefined
  /**
   * Choose a setting of the chat's agent, such as its model, by the
   * agent's own id. The chat's settings show it waiting until the agent
   * takes it.
   */
  setSetting(choice: SettingChoice): Promise<unknown> | void
  /**
   * Learn what settings the chat's agent offers, such as its models,
   * before its session opens: they come in the agent's status. No
   * message is sent.
   */
  learnSettings(): Promise<unknown> | void
}

/** What the user can do with the list of chats. */
export interface ListActions {
  /** Show this chat. */
  select(chat: ChatId): void
  /**
   * Open a new chat with an agent, in a folder (a full path), and choose
   * `settings` for it, such as its model, if any are given. Opening can't
   * carry them, so the host opens it, then chooses them: before its
   * session opens, any choice is taken, and applied as it opens, before
   * any message goes.
   */
  open(agent: AgentName, cwd: string, settings?: readonly SettingChoice[]): Promise<unknown> | void
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
 * Does something to a chat, `act`, such as unarchiving it or sending to
 * it, as a host's `ChatActions` does, restoring it first if it must. A
 * saved chat is put away as it's archived, so CAF no longer has it and
 * refuses with `NoSuchChat`: then, given `restore`, it's restored,
 * archived still, and `act` tried again. Without `restore`, or for any
 * other failure, the refusal is the caller's.
 */
export async function orRestored<T>(
  act: () => Promise<T>,
  restore?: (() => Promise<unknown>) | undefined,
): Promise<T> {
  try {
    return await act()
  } catch (error) {
    const gone = error instanceof CafError && error.why.type === 'Refused' && error.why.value.type === 'NoSuchChat'
    if (!gone || restore === undefined) throw error
    await restore()
    return await act()
  }
}

/**
 * Opens a chat with `open`, then chooses `settings` for it with `choose`,
 * as a host's `ListActions.open` does: opening can't carry them. It
 * tries every choice, one after another, and resolves once the chat is
 * open, with each that couldn't be chosen and why, in order. It rejects
 * only if the chat didn't open: a chat that opened is never opened again
 * for want of its settings. The host says why in the chat (`ChatView`'s
 * `notice`), where the user can choose them again.
 */
export async function openThenChoose<C>(
  open: () => Promise<C>,
  choose: (chat: C, choice: SettingChoice) => Promise<unknown>,
  settings: readonly SettingChoice[],
): Promise<{ chat: C; failed: { choice: SettingChoice; problem: unknown }[] }> {
  const chat = await open()
  const failed: { choice: SettingChoice; problem: unknown }[] = []
  for (const choice of settings) {
    const problem = await attempt(() => choose(chat, choice))
    if (problem !== null) failed.push({ choice, problem })
  }
  return { chat, failed }
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

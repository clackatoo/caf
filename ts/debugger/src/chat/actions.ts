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
  type CafTool,
  type ChatId,
  type OpenRequest,
  type DeepReadonly,
  type QuestionId,
  type Refused,
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
   * any message goes. `tools` are CAF's own tools to ask the host to give
   * it, which it may grant, trim or ignore: none if not given.
   */
  open(agent: AgentName, cwd: string, settings?: readonly SettingChoice[], tools?: readonly CafTool[]): Promise<unknown> | void
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
      if (!refused(error, 'Loaded') || tried >= tries) throw error
      await new Promise((done) => setTimeout(done, wait))
    }
  }
}

/**
 * Does something to a chat, `act`, such as unarchiving it or sending to
 * it, as a host's `ChatActions` does, restoring it first if it must. A
 * saved chat is put away as it's archived, so CAF no longer has it and
 * refuses with `NoSuchChat`: then, given `restore`, it's restored,
 * archived still, and `act` tried again. If CAF refuses to restore it as
 * loaded, another restored it meanwhile, so `act` is tried again all the
 * same; if CAF still has no such chat, that refusal to restore it says why
 * truer, and is the caller's. Without `restore`, or for any other failure,
 * the refusal is the caller's.
 */
export async function orRestored<T>(
  act: () => Promise<T>,
  restore?: (() => Promise<unknown>) | undefined,
): Promise<T> {
  try {
    return await act()
  } catch (error) {
    if (!refused(error, 'NoSuchChat') || restore === undefined) throw error
    let loaded: unknown = null
    try {
      await restore()
    } catch (problem) {
      if (!refused(problem, 'Loaded')) throw problem
      loaded = problem
    }
    try {
      return await act()
    } catch (again) {
      throw loaded !== null && refused(again, 'NoSuchChat') ? loaded : again
    }
  }
}

/** Runs `start` for `key`, unless one for it is under way: then gives back that one. */
export type OneAtATime<K> = <T>(key: K, start: () => Promise<T>) => Promise<T>

/**
 * Runs one at a time, by key: while one started for a key is under way,
 * starting another for it gives back the same, so two actions on one
 * chat, such as sending and unarchiving it, restore it once.
 */
export function oneAtATime<K>(): OneAtATime<K> {
  const running = new Map<K, Promise<unknown>>()
  return <T>(key: K, start: () => Promise<T>): Promise<T> => {
    const under = running.get(key)
    if (under !== undefined) return under as Promise<T>
    const started = start().finally(() => running.delete(key))
    running.set(key, started)
    return started
  }
}

/** Whether `error` is CAF refusing, for this reason. */
function refused(error: unknown, why: Refused['type']): boolean {
  return error instanceof CafError && error.why.type === 'Refused' && error.why.value.type === why
}

/**
 * What a host asks CAF for as it opens a new chat with `agent` in `cwd`,
 * giving it `tools` of CAF's own: none, `[]`, says so outright.
 */
export function newChat(agent: AgentName, cwd: string, tools: readonly CafTool[]): OpenRequest {
  return { agent, cwd, session: { type: 'New' }, tools: { caf: [...tools], app: [] } }
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

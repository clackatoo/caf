import type {
  AgentAction,
  AgentName,
  AgentStatus,
  Answer,
  BadQuery,
  CannotRestore,
  Chat,
  ChangeNo,
  ChatId,
  ChatSummary,
  FromViewer,
  ModelPrice,
  Numbered,
  OpenRequest,
  Outcome,
  QuestionId,
  Refused,
  Rights,
  SavedChat,
  SettingChoice,
  Shelf,
  Summary,
  TurnId,
  UsageQuery,
  UserMessage,
} from "./types.ts";
import { ChatCopy } from "./copy.ts";
import type { DeepReadonly } from "./readonly.ts";
import {
  isArrayOf,
  isChat,
  isNumber,
  isRecord,
  isRights,
  isSaved,
  isStatus,
  isString,
  isSummary,
  isTagged,
  isUsageSummary,
} from "./shape.ts";

/** The versions of the viewer wire this SDK speaks. */
const VERSIONS = { min: 1, max: 1 } as const;

/** The run of CAF each copy a connection made came from: see `Connection.run`. */
const runs = new WeakMap<ChatCopy, string>();

/** How messages travel: one JSON text per message, both ways. */
export interface Transport {
  send(text: string): void;
  onMessage(listener: (text: string) => void): void;
  /** Called once, when the link ends for any reason. */
  onClose(listener: () => void): void;
  close(): void;
}

/**
 * A transport over a WebSocket at `url`, once it's open. It rejects with
 * `Disconnected` if the socket closes before it opens. A binary message
 * isn't one CAF sends, so one closes the socket.
 */
export function webSocket(url: string): Promise<Transport> {
  return new Promise((resolve, reject) => {
    let socket: WebSocket;
    try {
      socket = new WebSocket(url);
    } catch {
      reject(new CafError({ type: "Disconnected" }));
      return;
    }
    const messages: ((text: string) => void)[] = [];
    const closes: (() => void)[] = [];
    let closed = false;
    const transport: Transport = {
      send: (text) => socket.send(text),
      onMessage: (listener) => void messages.push(listener),
      onClose: (listener) => {
        if (closed) queueMicrotask(() => call(listener, undefined));
        else closes.push(listener);
      },
      close: () => socket.close(),
    };
    socket.addEventListener("open", () => resolve(transport));
    socket.addEventListener("message", (event: MessageEvent) => {
      if (typeof event.data !== "string") return socket.close();
      for (const listener of messages) call(listener, event.data);
    });
    socket.addEventListener("close", () => {
      reject(new CafError({ type: "Disconnected" }));
      if (closed) return;
      closed = true;
      for (const listener of closes.splice(0)) call(listener, undefined);
    });
  });
}

/**
 * Why something asked of CAF wasn't done. Its message says it in short,
 * such as `Refused: QueueFull`.
 *
 * A later CAF may send a reason this SDK doesn't know, here in `Refused`
 * and `CannotRestore`, in a saved chat's `Failed`, and in a chat's `Lost`
 * and `FailureReason`. Give every `switch` on a reason a `default`, and
 * show one it doesn't know by its `type`.
 */
export class CafError extends Error {
  override readonly name = "CafError";

  constructor(
    readonly why:
      /** The core refused it, and nothing changed. A reason this SDK doesn't know comes by its `type`. */
      | { type: "Refused"; value: Refused }
      /** The host doesn't let viewers do this. */
      | { type: "NotAllowed" }
      /** CAF has shut down, or is shutting down: nothing was done. */
      | { type: "Gone" }
      /** The saved chat couldn't be restored, and its save is as it was. */
      | { type: "CannotRestore"; value: CannotRestore }
      /** CAF keeps no usage ledger, so has no summary to give, nor prices to set. */
      | { type: "NoLedger" }
      /** No summary can be made for the query asked, for this reason. */
      | { type: "BadQuery"; value: BadQuery }
      /** It went in a way this SDK doesn't know, from a later version of CAF: `outcome` is its name. */
      | { type: "Unknown"; value: { outcome: string } }
      /** The link ended before the answer came. */
      | { type: "Disconnected" }
      /** No version of the wire in common with CAF. */
      | { type: "NoCommonVersion"; value: { min: number; max: number } },
  ) {
    super(describe(why));
  }
}

/** A `CafError`'s message. */
function describe(why: CafError["why"]): string {
  switch (why.type) {
    case "Refused":
      return `Refused: ${why.value.type}`;
    case "CannotRestore":
      return `CannotRestore: ${why.value.type}`;
    case "BadQuery":
      return `BadQuery: ${why.value.type}`;
    case "Unknown":
      return `Unknown: ${why.value.outcome}`;
    case "NoCommonVersion":
      return `NoCommonVersion: CAF speaks versions ${why.value.min} to ${why.value.max}`;
    default:
      return why.type;
  }
}

/** Following one chat: see `Connection.watch`. */
export interface Watching {
  /**
   * The copy so far: null until the chat first comes. Pass it to `watch` on a new connection to the same run of CAF to
   * pick up where this left off.
   */
  readonly copy: ChatCopy | null;
  /** Stops following it. */
  stop(): void;
}

/** What `watch` was told, if anything, as it follows a chat. */
export type WatchEvent =
  /** The chat changed, or came whole: draw this. It's read-only all the way down: it shares its turns and items with the watch's copy. */
  | { type: "Chat"; chat: DeepReadonly<Chat> }
  /** The chat was closed, or CAF shut down: nothing more comes. */
  | { type: "Ended" };

/**
 * A link to CAF: the viewer wire, version 1. Pending requests reject with
 * `Disconnected` when the link ends. Reconnecting is the app's choice: open
 * a new connection, and watch each chat again `from` its last copy, if CAF
 * hasn't restarted meanwhile (see `run`).
 *
 * Nothing CAF sends makes it throw:
 *
 * - A message of a type it doesn't know, from a later version of the wire,
 *   is ignored.
 * - A change it can't apply, of a type it doesn't know or one that doesn't
 *   fit its copy, makes it ask for the chat whole again.
 * - A request answered with an outcome it doesn't know rejects with
 *   `Unknown`, carrying the outcome's name, and the link goes on.
 * - Anything else that isn't what CAF sends ends the link, as if it had
 *   dropped: text that isn't JSON, a message with no `type`, a message it
 *   knows without the fields it needs, a `Welcome` after the first, or an
 *   answer to a request that doesn't fit what was asked. Ignoring such a
 *   message could leave a request waiting forever or a chat silently
 *   wrong, so the app hears `onClose` instead, every pending request
 *   rejects with `Disconnected`, and the app can connect again.
 *
 * A listener of the app's that throws doesn't end the link: the error is
 * reported as uncaught (with `reportError`, where there is one), and the
 * other listeners are still called.
 *
 * A later CAF may also send reasons this SDK doesn't know, inside messages
 * it does: a `Refused` or `CannotRestore`, or a chat's `Lost` or
 * `FailureReason`. Give every `switch` on a reason a `default`. See
 * `CafError`.
 */
export class Connection {
  readonly #transport: Transport;
  #agents: readonly DeepReadonly<AgentStatus>[];
  readonly #run: string;
  readonly #may: Readonly<Rights>;
  #chats: readonly Readonly<ChatSummary>[];
  #saved: readonly DeepReadonly<SavedChat>[] = [];
  readonly #savedListeners = new Set<(saved: readonly DeepReadonly<SavedChat>[]) => void>();
  readonly #chatListeners = new Set<(chats: readonly Readonly<ChatSummary>[]) => void>();
  readonly #agentListeners = new Set<(agents: readonly DeepReadonly<AgentStatus>[]) => void>();
  readonly #closeListeners = new Set<() => void>();
  #closed = false;
  #nextRequest = 1;
  readonly #pending = new Map<number, Pending>();
  readonly #watches = new Map<ChatId, Set<Watch>>();
  /** Chats CAF has been asked for whole, and hasn't sent yet. */
  readonly #asked = new Set<ChatId>();

  private constructor(
    transport: Transport,
    agents: readonly AgentStatus[],
    chats: readonly ChatSummary[],
    run: string,
    may: Rights,
  ) {
    this.#transport = transport;
    this.#agents = agents;
    this.#chats = chats;
    this.#run = run;
    this.#may = { act: may.act, agents: may.agents, usage: may.usage ?? false };
  }

  /**
   * Says hello over `transport`, and resolves once CAF has welcomed it and
   * said where its agents stand, which it does next, so `agents` is known
   * from the start. Rejects with `NoCommonVersion` if CAF speaks no
   * version this SDK does, and with `Disconnected` if the link ends first,
   * or CAF answers with anything else.
   *
   * It waits as long as CAF takes to answer. Given a `signal`, aborting it
   * before both have come rejects with the signal's reason and closes the
   * transport; aborting it after does nothing.
   */
  static open(transport: Transport, options?: { signal?: AbortSignal }): Promise<Connection> {
    return new Promise((resolve, reject) => {
      const signal = options?.signal;
      if (signal?.aborted) {
        reject(signal.reason);
        transport.close();
        return;
      }
      let connection: Connection | null = null;
      /** What the Welcome said, while the agents' statuses are awaited. */
      let welcomed: { chats: ChatSummary[]; run: string; may: Rights } | null = null;
      let settled = false;
      const settle = () => {
        settled = true;
        signal?.removeEventListener("abort", abort);
      };
      const fail = (why: CafError["why"]) => {
        if (settled) return;
        settle();
        reject(new CafError(why));
        transport.close();
      };
      const abort = () => {
        if (settled) return;
        settle();
        reject(signal?.reason);
        transport.close();
      };
      signal?.addEventListener("abort", abort);
      const welcome = (text: string) => {
        const message = parse(text);
        if (message === null) return fail({ type: "Disconnected" });
        const value = message.value;
        if (welcomed !== null) {
          switch (message.type) {
            case "Agents": {
              if (!isArrayOf(value, isStatus)) return fail({ type: "Disconnected" });
              settle();
              connection = new Connection(transport, value, welcomed.chats, welcomed.run, welcomed.may);
              return resolve(connection);
            }
            default:
              // As before the Welcome: a later version's message is
              // ignored, and any other of this version's breaks the
              // handshake.
              if (KNOWN.has(message.type)) return fail({ type: "Disconnected" });
              return;
          }
        }
        switch (message.type) {
          case "Welcome": {
            if (!isRecord(value)) return fail({ type: "Disconnected" });
            const { version, agents, chats, run, may } = value;
            // CAF chooses one both speak, or it's broken.
            if (!isNumber(version) || version < VERSIONS.min || version > VERSIONS.max)
              return fail({ type: "Disconnected" });
            if (!isArrayOf(agents, isString) || !isArrayOf(chats, isSummary) || !isString(run) || !isRights(may)) {
              return fail({ type: "Disconnected" });
            }
            // Its agents' names come again, with their statuses, next.
            welcomed = { chats, run, may };
            return;
          }
          case "NoCommonVersion": {
            if (!isRecord(value) || !isNumber(value.min) || !isNumber(value.max)) return fail({ type: "Disconnected" });
            return fail({ type: "NoCommonVersion", value: { min: value.min, max: value.max } });
          }
          default:
            // A later version's message is ignored; any other of this
            // version's breaks the handshake.
            if (KNOWN.has(message.type)) return fail({ type: "Disconnected" });
        }
      };
      transport.onMessage((text) => {
        try {
          if (connection !== null) connection.#hear(text);
          else if (!settled) welcome(text);
        } catch {
          // Only a bug of this SDK's gets here: end the link, never throw
          // into the transport.
          if (connection !== null) connection.#end();
          else fail({ type: "Disconnected" });
        }
      });
      transport.onClose(() => {
        if (connection !== null) connection.#end();
        else fail({ type: "Disconnected" });
      });
      const hello: FromViewer = { type: "Hello", value: VERSIONS };
      try {
        transport.send(JSON.stringify(hello));
      } catch {
        fail({ type: "Disconnected" });
      }
    });
  }

  /**
   * Every agent on offer, in the order the host offers them: those a chat
   * may be opened with, each installed or not, and signed in or not.
   * Read-only.
   */
  get agents(): readonly DeepReadonly<AgentStatus>[] {
    return this.#agents;
  }

  /** The open chats, in the order they were opened. Read-only. */
  get chats(): readonly Readonly<ChatSummary>[] {
    return this.#chats;
  }

  /**
   * The saved chats on the shelves the host has listed, loaded or not, in
   * the order they were made: empty until CAF says, which it does right
   * after the agents if the host has listed any, so possibly only after
   * `open` has resolved: follow it with `onSaved`. Read-only.
   */
  get saved(): readonly DeepReadonly<SavedChat>[] {
    return this.#saved;
  }

  /**
   * This run of CAF: different every time CAF starts. A chat keeps its id
   * when it's saved and restored, but a restarted CAF numbers its changes
   * from the start again, so a copy is only resumed from on a connection
   * to the run it came from.
   */
  get run(): string {
    return this.#run;
  }

  /**
   * What the host lets this viewer do: `act` on chats (send, stop, answer,
   * choose their settings and close), and on `agents` (install, sign in
   * and out, and learn their settings). Offer only what it may: anything
   * else is answered `NotAllowed`. A viewer that may not act on agents
   * gets no sign-in's link or code. Whether it may open chats, or restore
   * saved ones, isn't said, as the host decides each request.
   */
  get may(): Readonly<Rights> {
    return this.#may;
  }

  /** Calls `listener` with the open chats each time the list changes. Returns a function that stops it. */
  onChats(listener: (chats: readonly Readonly<ChatSummary>[]) => void): () => void {
    // Each call is its own subscription, even of the same function.
    const each = (chats: readonly Readonly<ChatSummary>[]) => listener(chats);
    this.#chatListeners.add(each);
    return () => void this.#chatListeners.delete(each);
  }

  /**
   * Calls `listener` with every agent's status each time any of it
   * changes. Returns a function that stops it.
   */
  onAgents(listener: (agents: readonly DeepReadonly<AgentStatus>[]) => void): () => void {
    const each = (agents: readonly DeepReadonly<AgentStatus>[]) => listener(agents);
    this.#agentListeners.add(each);
    return () => void this.#agentListeners.delete(each);
  }

  /**
   * Calls `listener` with the saved chats each time the list changes.
   * Returns a function that stops it.
   */
  onSaved(listener: (saved: readonly DeepReadonly<SavedChat>[]) => void): () => void {
    const each = (saved: readonly DeepReadonly<SavedChat>[]) => listener(saved);
    this.#savedListeners.add(each);
    return () => void this.#savedListeners.delete(each);
  }

  /**
   * Calls `listener` once, when the link ends: soon, if it has already.
   * Returns a function that stops it.
   */
  onClose(listener: () => void): () => void {
    let stopped = false;
    const each = () => {
      if (!stopped) listener();
    };
    if (this.#closed) queueMicrotask(() => call(each, undefined));
    else this.#closeListeners.add(each);
    return () => {
      stopped = true;
      this.#closeListeners.delete(each);
    };
  }

  /**
   * Follows a chat, calling `listener` with it whole each time it changes.
   * Gaps, pictures and changes of a type this SDK doesn't know are handled
   * inside: the listener only ever sees the right chat. Given `from`, a copy
   * from an earlier connection, CAF sends only what changed since, if it
   * still can.
   *
   * Given `from`, the listener first hears of the chat once something
   * changes; until then `from.chat` is current. `from` is only resumed
   * from if it's a watch's copy of this chat, from a connection to this
   * run of CAF (see `run`); otherwise CAF sends the chat whole. A chat that
   * doesn't exist is `Ended` at once. Watching a chat this connection
   * already watches gets every watch of it the chat whole again. The link
   * ending doesn't end a watch: `onClose` says so.
   *
   * Throws `Disconnected` if the link has ended.
   */
  watch(chat: ChatId, listener: (event: WatchEvent) => void, from?: ChatCopy): Watching {
    if (this.#closed) throw new CafError({ type: "Disconnected" });
    const run = this.#run;
    const resumable = from !== undefined && from.chat.id === chat && runs.get(from) === run;
    const copy = resumable ? made(new ChatCopy(from.chat, from.latest), run) : null;
    const link: Link = {
      run,
      forget: (watch) => this.#forget(watch),
      rewatch: (chat) => this.#rewatch(chat),
    };
    const watch = new Watch(chat, listener, copy, link);
    let watches = this.#watches.get(chat);
    // CAF keeps one watch of a chat per link, and a second Watch starts
    // it again: ask for a picture, so the watches already waiting for one
    // get it too.
    const since = watches === undefined && copy !== null ? copy.latest : null;
    if (watches === undefined) this.#watches.set(chat, (watches = new Set()));
    watches.add(watch);
    if (since === null) this.#asked.add(chat);
    this.#send({ type: "Watch", value: { chat, since } });
    return watch;
  }

  /** Asks to open a chat. The host decides: it may reject with `NotAllowed`. */
  open(request: OpenRequest): Promise<ChatId> {
    return this.#request("Opened", (no) => ({ type: "Open", value: { request: no, open: request } }));
  }

  /**
   * Asks to restore a saved chat, as `saved` lists them, resolving with its
   * id once it's loaded. The host decides: it may reject with
   * `NotAllowed`. It rejects with `CannotRestore` saying why if the chat
   * can't be, and its save is left as it was.
   */
  restore(chat: ChatId, shelf: Shelf): Promise<ChatId> {
    return this.#request("Opened", (no) => ({ type: "Restore", value: { request: no, chat, shelf } }));
  }

  /**
   * Chooses a setting of a chat's agent, such as its model, by the
   * agent's own id: the chat's `settings` show it waiting until the agent
   * answers. A change made while the agent is answering applies from the
   * next message. It rejects with `Refused` `NoSuchSetting` if the agent
   * has said what it offers and that isn't among it; before it has, any
   * choice is taken, and checked as the session opens. The host may
   * reject it with `NotAllowed`.
   */
  setSetting(chat: ChatId, choice: SettingChoice): Promise<void> {
    return this.#request("Done", (no) => ({ type: "Set", value: { request: no, chat, choice } }));
  }

  /** Sends a message, resolving with its turn once the core has taken it. */
  send(chat: ChatId, message: UserMessage): Promise<TurnId> {
    return this.#request("Sent", (no) => ({ type: "Send", value: { request: no, chat, message } }));
  }

  /** Stops the chat's turn. */
  stop(chat: ChatId): Promise<void> {
    return this.#request("Done", (no) => ({ type: "Stop", value: { request: no, chat } }));
  }

  /** Answers a question. */
  answer(question: QuestionId, answer: Answer): Promise<void> {
    return this.#request("Done", (no) => ({ type: "Answer", value: { request: no, question, answer } }));
  }

  /** Closes a chat. */
  close(chat: ChatId): Promise<void> {
    return this.#request("Done", (no) => ({ type: "Close", value: { request: no, chat } }));
  }

  /**
   * Installs an agent CAF installs itself, at the version its profile was
   * checked at. It resolves once CAF has started: `agents` follows the
   * install, and chats waiting for the agent start once it's done. The
   * host decides whether viewers may do this and the rest below: it may
   * reject with `NotAllowed`.
   */
  install(agent: AgentName): Promise<void> {
    return this.#agent(agent, { type: "Install" });
  }

  /** Stops installing an agent. Chats waiting for it stop their waiting messages, and go on waiting. */
  stopInstall(agent: AgentName): Promise<void> {
    return this.#agent(agent, { type: "StopInstall" });
  }

  /**
   * Starts signing in to an agent, with CAF's own command for it. Its
   * status in `agents` then gives the link to open, and any code to type.
   */
  signIn(agent: AgentName): Promise<void> {
    return this.#agent(agent, { type: "SignIn" });
  }

  /** Gives an agent's sign-in the code the user pasted, where its status allows a paste. */
  paste(agent: AgentName, text: string): Promise<void> {
    return this.#agent(agent, { type: "Paste", value: text });
  }

  /** Stops signing in to an agent. */
  stopSignIn(agent: AgentName): Promise<void> {
    return this.#agent(agent, { type: "StopSignIn" });
  }

  /** Signs out of an agent. */
  signOut(agent: AgentName): Promise<void> {
    return this.#agent(agent, { type: "SignOut" });
  }

  /**
   * Learns what settings an agent offers, such as its models, to offer
   * them before any chat with it: they show in `agents`. It costs nothing:
   * no message is sent.
   */
  learnSettings(agent: AgentName): Promise<void> {
    return this.#agent(agent, { type: "LearnSettings" });
  }

  /**
   * Asks for usage over a period, from every CAF app on the machine: what
   * a usage page is drawn from. Group it with `usage.ts`'s helpers. It
   * rejects with `NotAllowed` unless `may.usage`, `NoLedger` if CAF keeps
   * no ledger, and `BadQuery` saying why for a query that can't be
   * answered. Ask again for newer figures: summaries aren't pushed.
   */
  usage(query: UsageQuery): Promise<Summary> {
    return this.#request("Summary", (no) => ({ type: "Usage", value: { request: no, query } }));
  }

  /**
   * Sets the user's own price for a model, by the agent's id for it, or
   * with `null` clears it, for every CAF app on the machine. Every chat's
   * `spend` is sent again with it. It rejects with `NotAllowed` unless
   * `may.usage`, or `NoLedger`.
   */
  setPrice(model: string, price: ModelPrice | null): Promise<void> {
    return this.#request("Done", (no) => ({ type: "SetPrice", value: { request: no, model, price } }));
  }

  /** Ends the link. */
  disconnect(): void {
    this.#end();
  }

  /** Asks CAF to do something about an agent. */
  #agent(agent: AgentName, action: AgentAction): Promise<void> {
    return this.#request("Done", (no) => ({ type: "Agent", value: { request: no, agent, action } }));
  }

  /** Asks CAF something, resolving with the outcome it expects. */
  #request<E extends Answered>(expect: E, message: (request: number) => FromViewer): Promise<AnswerOf<E>> {
    return new Promise((resolve, reject) => {
      if (this.#closed) return reject(new CafError({ type: "Disconnected" }));
      const request = this.#nextRequest++;
      const sent = message(request);
      const pending: Pending = { asked: sent.type, expect, resolve: resolve as (value: unknown) => void, reject };
      this.#pending.set(request, pending);
      this.#send(sent);
    });
  }

  /** Sends a message, ending the link if the transport can't. */
  #send(message: FromViewer): void {
    if (this.#closed) return;
    try {
      this.#transport.send(JSON.stringify(message));
    } catch {
      this.#end();
    }
  }

  /** One message from CAF, after the handshake. */
  #hear(text: string): void {
    if (this.#closed) return;
    const message = parse(text);
    if (message === null) return this.#end();
    const value = message.value;
    switch (message.type) {
      case "Chats": {
        if (!isArrayOf(value, isSummary)) return this.#end();
        this.#chats = value;
        for (const listener of [...this.#chatListeners]) call(listener, value);
        return;
      }
      case "Agents": {
        if (!isArrayOf(value, isStatus)) return this.#end();
        this.#agents = value;
        for (const listener of [...this.#agentListeners]) call(listener, value);
        return;
      }
      case "Saved": {
        if (!isArrayOf(value, isSaved)) return this.#end();
        this.#saved = value;
        for (const listener of [...this.#savedListeners]) call(listener, value);
        return;
      }
      case "Picture": {
        if (!isRecord(value) || !isChat(value.chat) || !isNumber(value.latest)) return this.#end();
        const { chat, latest } = value;
        this.#asked.delete(chat.id);
        for (const watch of this.#watchesOf(chat.id)) watch.picture(chat, latest);
        return;
      }
      case "Changes": {
        if (!isRecord(value) || !isString(value.chat) || !Array.isArray(value.changes)) return this.#end();
        const changes = value.changes as Numbered[];
        for (const watch of this.#watchesOf(value.chat)) watch.changes(changes);
        return;
      }
      case "Ended": {
        if (!isRecord(value) || !isString(value.chat)) return this.#end();
        const watches = this.#watchesOf(value.chat);
        this.#watches.delete(value.chat);
        this.#asked.delete(value.chat);
        for (const watch of watches) watch.ended();
        return;
      }
      case "Done": {
        const outcome = isRecord(value) ? value.outcome : undefined;
        if (!isRecord(value) || !isNumber(value.request) || !isRecord(outcome) || !isString(outcome.type)) {
          return this.#end();
        }
        const pending = this.#pending.get(value.request);
        // An answer to nothing asked: CAF never sends one, but it harms
        // nothing.
        if (pending === undefined) return;
        this.#pending.delete(value.request);
        return this.#answer(pending, outcome.type, outcome.value);
      }
      case "Usage": {
        if (!isRecord(value) || !isNumber(value.request)) return this.#end();
        const pending = this.#pending.get(value.request);
        if (pending === undefined) return;
        this.#pending.delete(value.request);
        // A summary answers only a request for one, and must hold what
        // the helpers read.
        if (pending.expect !== "Summary" || !isUsageSummary(value.summary)) {
          pending.reject(new CafError({ type: "Disconnected" }));
          return this.#end();
        }
        return pending.resolve(value.summary);
      }
      case "Welcome":
      case "NoCommonVersion":
        return this.#end();
      default:
        // A message of a later version of the wire.
        return;
    }
  }

  /**
   * Settles a request with its outcome. One of a later version's rejects
   * it as `Unknown`, as the wire says an unknown reason is shown. One that
   * doesn't fit what was asked, or lacks what it holds, ends the link:
   * neither resolving nor rejecting would be the truth, so it rejects as
   * the link does.
   */
  #answer(pending: Pending, type: string, value: unknown): void {
    const broken = () => {
      pending.reject(new CafError({ type: "Disconnected" }));
      this.#end();
    };
    switch (type) {
      case "Refused":
        if (!isTagged(value)) return broken();
        return pending.reject(new CafError({ type: "Refused", value: value as Refused }));
      case "NotAllowed":
        return pending.reject(new CafError({ type: "NotAllowed" }));
      case "Gone":
        return pending.reject(new CafError({ type: "Gone" }));
      case "CannotRestore":
        if (pending.asked !== "Restore" || !isTagged(value)) return broken();
        return pending.reject(new CafError({ type: "CannotRestore", value: value as CannotRestore }));
      case "NoLedger":
        if (pending.asked !== "Usage" && pending.asked !== "SetPrice") return broken();
        return pending.reject(new CafError({ type: "NoLedger" }));
      case "BadQuery":
        if (pending.asked !== "Usage" || !isTagged(value)) return broken();
        return pending.reject(new CafError({ type: "BadQuery", value: value as BadQuery }));
      case pending.expect:
        // Not an outcome of this version's: a summary comes in a `Usage`.
        if (pending.expect === "Summary") return pending.reject(new CafError({ type: "Unknown", value: { outcome: type } }));
        if (pending.expect === "Done") return pending.resolve(undefined);
        // A chat's id is a UUID's text; a turn's is a number.
        if (pending.expect === "Opened" ? !isString(value) : !isNumber(value)) return broken();
        return pending.resolve(value);
      default:
        if (OUTCOMES.has(type)) return broken();
        return pending.reject(new CafError({ type: "Unknown", value: { outcome: type } }));
    }
  }

  /** The watches of a chat, as they are now. */
  #watchesOf(chat: ChatId): Watch[] {
    return [...(this.#watches.get(chat) ?? [])];
  }

  /** A watch has stopped: stop watching its chat, if it was the last. */
  #forget(watch: Watch): void {
    const watches = this.#watches.get(watch.chat);
    if (watches === undefined || !watches.delete(watch) || watches.size > 0) return;
    this.#watches.delete(watch.chat);
    this.#asked.delete(watch.chat);
    this.#send({ type: "Unwatch", value: { chat: watch.chat } });
  }

  /** Asks for a chat whole again, unless it has been already. */
  #rewatch(chat: ChatId): void {
    if (this.#asked.has(chat)) return;
    this.#asked.add(chat);
    this.#send({ type: "Unwatch", value: { chat } });
    this.#send({ type: "Watch", value: { chat, since: null } });
  }

  /** The link has ended, by either side: settle everything, once. */
  #end(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#watches.clear();
    this.#asked.clear();
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const each of pending) each.reject(new CafError({ type: "Disconnected" }));
    const closes = [...this.#closeListeners];
    this.#closeListeners.clear();
    for (const listener of closes) call(listener, undefined);
    this.#chatListeners.clear();
    this.#agentListeners.clear();
    this.#savedListeners.clear();
    try {
      this.#transport.close();
    } catch {
      // It's ended either way.
    }
  }
}

/**
 * The outcomes a request may expect, and what each resolves with. A
 * `Summary` isn't an outcome: it comes in a `Usage` message of its own,
 * in place of a `Done`.
 */
type Answered = "Opened" | "Sent" | "Done" | "Summary";
type AnswerOf<E extends Answered> = E extends "Done"
  ? void
  : E extends "Summary"
    ? Summary
    : Extract<Outcome, { type: E }> extends { value: infer V }
      ? V
      : never;

/** A request waiting for its answer: a `Done`, or for a summary, a `Usage`. */
interface Pending {
  /** What was asked: an Open and a Restore both expect `Opened`, and only a Restore may be answered `CannotRestore`. */
  asked: FromViewer["type"];
  expect: Answered;
  resolve(value: unknown): void;
  reject(error: CafError): void;
}

/** What a watch needs of its connection. */
interface Link {
  /** The run of CAF the connection is to. */
  readonly run: string;
  forget(watch: Watch): void;
  rewatch(chat: ChatId): void;
}

/** One watch of a chat, as `Connection.watch` gives it. */
class Watch implements Watching {
  readonly chat: ChatId;
  readonly #listener: (event: WatchEvent) => void;
  readonly #link: Link;
  #copy: ChatCopy | null;
  /** It has asked for the chat whole: changes before it comes don't follow from its copy. */
  #waiting: boolean;
  #stopped = false;

  constructor(chat: ChatId, listener: (event: WatchEvent) => void, copy: ChatCopy | null, link: Link) {
    this.chat = chat;
    this.#listener = listener;
    this.#link = link;
    this.#copy = copy;
    this.#waiting = copy === null;
  }

  get copy(): ChatCopy | null {
    return this.#copy;
  }

  stop(): void {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#link.forget(this);
  }

  picture(chat: Chat, latest: ChangeNo): void {
    if (this.#stopped) return;
    this.#copy = made(new ChatCopy(chat, latest), this.#link.run);
    this.#waiting = false;
    call(this.#listener, { type: "Chat", chat });
  }

  changes(changes: readonly Numbered[]): void {
    const copy = this.#copy;
    if (this.#stopped || this.#waiting || copy === null) return;
    const before = copy.chat;
    const taken = copy.take(changes);
    // What it took before it couldn't is still the chat as it was then.
    // It's drawn before asking again, since a transport may hand over the
    // new picture before the ask returns.
    if (copy.chat !== before) call(this.#listener, { type: "Chat", chat: copy.chat });
    // Unless the listener stopped the watch, or a picture came meanwhile.
    if (taken === "need-picture" && !this.#stopped && this.#copy === copy) {
      this.#waiting = true;
      this.#link.rewatch(this.chat);
    }
  }

  ended(): void {
    if (this.#stopped) return;
    this.#stopped = true;
    call(this.#listener, { type: "Ended" });
  }
}

/** The outcomes this SDK knows. */
const OUTCOMES = new Set([
  "Opened",
  "Sent",
  "Done",
  "Refused",
  "NotAllowed",
  "Gone",
  "CannotRestore",
  "NoLedger",
  "BadQuery",
]);

/** The message types of version 1 of the wire, from CAF. */
const KNOWN = new Set([
  "Welcome",
  "NoCommonVersion",
  "Chats",
  "Picture",
  "Changes",
  "Ended",
  "Agents",
  "Saved",
  "Done",
  "Usage",
]);

/** A message from CAF as far as `{ type, value }`, or null if it isn't one. */
function parse(text: unknown): { type: string; value: unknown } | null {
  if (typeof text !== "string") return null;
  let message: unknown;
  try {
    message = JSON.parse(text);
  } catch {
    return null;
  }
  if (!isRecord(message) || !isString(message.type)) return null;
  return { type: message.type, value: message.value };
}

/** Remembers that `copy` came from `run`. */
function made(copy: ChatCopy, run: string): ChatCopy {
  runs.set(copy, run);
  return copy;
}

/** Calls an app's listener, reporting what it throws rather than letting it end the link. */
function call<T>(listener: (value: T) => void, value: T): void {
  try {
    listener(value);
  } catch (error) {
    if (typeof globalThis.reportError === "function") globalThis.reportError(error);
    else
      queueMicrotask(() => {
        throw error;
      });
  }
}

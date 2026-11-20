import type {
  AgentName,
  Answer,
  Chat,
  ChangeNo,
  ChatId,
  ChatSummary,
  FromViewer,
  Numbered,
  OpenRequest,
  Outcome,
  QuestionId,
  Refused,
  TurnId,
  UserMessage,
} from "./types.ts";
import { ChatCopy } from "./copy.ts";

/** The versions of the viewer wire this SDK speaks. */
const VERSIONS = { min: 1, max: 1 } as const;

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

/** Why something asked of CAF wasn't done. */
export class CafError extends Error {
  constructor(
    readonly why:
      /** The core refused it, and nothing changed. A reason this SDK doesn't know comes by its `type`. */
      | { type: "Refused"; value: Refused }
      /** The host doesn't let viewers do this. */
      | { type: "NotAllowed" }
      /** CAF has shut down, or is shutting down: nothing was done. */
      | { type: "Gone" }
      /** It went in a way this SDK doesn't know, from a later version of CAF: `outcome` is its name. */
      | { type: "Unknown"; value: { outcome: string } }
      /** The link ended before the answer came. */
      | { type: "Disconnected" }
      /** No version of the wire in common with CAF. */
      | { type: "NoCommonVersion"; value: { min: number; max: number } },
  ) {
    super(why.type);
  }
}

/** Following one chat: see `Connection.watch`. */
export interface Watching {
  /** The copy so far: null until the chat first comes. Pass it to `watch` on a new connection to pick up where this left off. */
  readonly copy: ChatCopy | null;
  /** Stops following it. */
  stop(): void;
}

/** What `watch` was told, if anything, as it follows a chat. */
export type WatchEvent =
  /** The chat changed, or came whole: draw this. */
  | { type: "Chat"; chat: Chat }
  /** The chat was closed, or CAF shut down: nothing more comes. */
  | { type: "Ended" };

/**
 * A link to CAF: the viewer wire, version 1. Pending requests reject with
 * `Disconnected` when the link ends. Reconnecting is the app's choice: open
 * a new connection, and watch each chat again `from` its last copy.
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
 */
export class Connection {
  readonly #transport: Transport;
  readonly #agents: readonly AgentName[];
  #chats: readonly ChatSummary[];
  readonly #chatListeners = new Set<(chats: readonly ChatSummary[]) => void>();
  readonly #closeListeners: (() => void)[] = [];
  #closed = false;
  #nextRequest = 1;
  readonly #pending = new Map<number, Pending>();
  readonly #watches = new Map<ChatId, Set<Watch>>();
  /** Chats CAF has been asked for whole, and hasn't sent yet. */
  readonly #asked = new Set<ChatId>();

  private constructor(transport: Transport, agents: readonly AgentName[], chats: readonly ChatSummary[]) {
    this.#transport = transport;
    this.#agents = agents;
    this.#chats = chats;
  }

  /**
   * Says hello over `transport`, and resolves once CAF welcomes it. Rejects
   * with `NoCommonVersion` if CAF speaks no version this SDK does, and with
   * `Disconnected` if the link ends first, or CAF answers with anything
   * else.
   */
  static open(transport: Transport): Promise<Connection> {
    return new Promise((resolve, reject) => {
      let connection: Connection | null = null;
      let settled = false;
      const fail = (why: CafError["why"]) => {
        if (settled) return;
        settled = true;
        reject(new CafError(why));
        transport.close();
      };
      const welcome = (text: string) => {
        const message = parse(text);
        if (message === null) return fail({ type: "Disconnected" });
        const value = message.value;
        switch (message.type) {
          case "Welcome": {
            if (!isRecord(value)) return fail({ type: "Disconnected" });
            const { version, agents, chats } = value;
            // CAF chooses one both speak, or it's broken.
            if (!isNumber(version) || version < VERSIONS.min || version > VERSIONS.max)
              return fail({ type: "Disconnected" });
            if (!isArrayOf(agents, isString) || !isArrayOf(chats, isSummary)) return fail({ type: "Disconnected" });
            settled = true;
            connection = new Connection(transport, agents, chats);
            return resolve(connection);
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

  /** The agents a chat may be opened with. */
  get agents(): readonly AgentName[] {
    return this.#agents;
  }

  /** The open chats, in the order they were opened. */
  get chats(): readonly ChatSummary[] {
    return this.#chats;
  }

  /** Calls `listener` with the open chats each time the list changes. Returns a function that stops it. */
  onChats(listener: (chats: readonly ChatSummary[]) => void): () => void {
    // Each call is its own subscription, even of the same function.
    const each = (chats: readonly ChatSummary[]) => listener(chats);
    this.#chatListeners.add(each);
    return () => void this.#chatListeners.delete(each);
  }

  /** Calls `listener` once, when the link ends: soon, if it has already. */
  onClose(listener: () => void): void {
    if (this.#closed) queueMicrotask(() => call(listener, undefined));
    else this.#closeListeners.push(listener);
  }

  /**
   * Follows a chat, calling `listener` with it whole each time it changes.
   * Gaps, pictures and changes of a type this SDK doesn't know are handled
   * inside: the listener only ever sees the right chat. Given `from`, a copy
   * from an earlier connection, CAF sends only what changed since, if it
   * still can.
   *
   * Given `from`, the listener first hears of the chat once something
   * changes; until then `from.chat` is current. A `from` of another chat is
   * ignored. A chat that doesn't exist is `Ended` at once. Watching a chat
   * this connection already watches gets every watch of it the chat whole
   * again. The link ending doesn't end a watch: `onClose` says so.
   */
  watch(chat: ChatId, listener: (event: WatchEvent) => void, from?: ChatCopy): Watching {
    const copy = from !== undefined && from.chat.id === chat ? new ChatCopy(from.chat, from.latest) : null;
    const link: Link = {
      forget: (watch) => this.#forget(watch),
      rewatch: (chat) => this.#rewatch(chat),
    };
    const watch = new Watch(chat, listener, copy, link);
    if (this.#closed) {
      watch.stop();
      return watch;
    }
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

  /** Ends the link. */
  disconnect(): void {
    this.#end();
  }

  /** Asks CAF something, resolving with the outcome it expects. */
  #request<E extends Answered>(expect: E, message: (request: number) => FromViewer): Promise<AnswerOf<E>> {
    return new Promise((resolve, reject) => {
      if (this.#closed) return reject(new CafError({ type: "Disconnected" }));
      const request = this.#nextRequest++;
      const pending: Pending = { expect, resolve: resolve as (value: unknown) => void, reject };
      this.#pending.set(request, pending);
      this.#send(message(request));
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
      case "Picture": {
        if (!isRecord(value) || !isChat(value.chat) || !isNumber(value.latest)) return this.#end();
        const { chat, latest } = value;
        this.#asked.delete(chat.id);
        for (const watch of this.#watchesOf(chat.id)) watch.picture(chat, latest);
        return;
      }
      case "Changes": {
        if (!isRecord(value) || !isNumber(value.chat) || !Array.isArray(value.changes)) return this.#end();
        const changes = value.changes as Numbered[];
        for (const watch of this.#watchesOf(value.chat)) watch.changes(changes);
        return;
      }
      case "Ended": {
        if (!isRecord(value) || !isNumber(value.chat)) return this.#end();
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
        if (!isRecord(value) || !isString(value.type)) return broken();
        return pending.reject(new CafError({ type: "Refused", value: value as Refused }));
      case "NotAllowed":
        return pending.reject(new CafError({ type: "NotAllowed" }));
      case "Gone":
        return pending.reject(new CafError({ type: "Gone" }));
      case pending.expect:
        if (pending.expect === "Done") return pending.resolve(undefined);
        if (!isNumber(value)) return broken();
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
    for (const listener of this.#closeListeners.splice(0)) call(listener, undefined);
    this.#chatListeners.clear();
    try {
      this.#transport.close();
    } catch {
      // It's ended either way.
    }
  }
}

/** The outcomes a request may expect, and what each resolves with. */
type Answered = "Opened" | "Sent" | "Done";
type AnswerOf<E extends Answered> = E extends "Done"
  ? void
  : Extract<Outcome, { type: E }> extends { value: infer V }
    ? V
    : never;

/** A request waiting for its `Done`. */
interface Pending {
  expect: Answered;
  resolve(value: unknown): void;
  reject(error: CafError): void;
}

/** What a watch needs of its connection. */
interface Link {
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
    this.#copy = new ChatCopy(chat, latest);
    this.#waiting = false;
    call(this.#listener, { type: "Chat", chat });
  }

  changes(changes: readonly Numbered[]): void {
    const copy = this.#copy;
    if (this.#stopped || this.#waiting || copy === null) return;
    const before = copy.chat;
    const taken = copy.take(changes);
    if (taken === "need-picture") {
      this.#waiting = true;
      this.#link.rewatch(this.chat);
    }
    // What it took before it couldn't is still the chat as it was then.
    if (copy.chat !== before && !this.#stopped) call(this.#listener, { type: "Chat", chat: copy.chat });
  }

  ended(): void {
    if (this.#stopped) return;
    this.#stopped = true;
    call(this.#listener, { type: "Ended" });
  }
}

/** The outcomes this SDK knows. */
const OUTCOMES = new Set(["Opened", "Sent", "Done", "Refused", "NotAllowed", "Gone"]);

/** The message types of version 1 of the wire, from CAF. */
const KNOWN = new Set(["Welcome", "NoCommonVersion", "Chats", "Picture", "Changes", "Ended", "Done"]);

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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isNumber(value: unknown): value is number {
  return typeof value === "number";
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function isArrayOf<T>(value: unknown, each: (value: unknown) => value is T): value is T[] {
  return Array.isArray(value) && value.every(each);
}

function isSummary(value: unknown): value is ChatSummary {
  return isRecord(value) && isNumber(value.id) && isString(value.agent) && isString(value.cwd);
}

function hasId(value: unknown): value is Record<string, unknown> & { id: number } {
  return isRecord(value) && isNumber(value.id);
}

/**
 * Whether a chat has the shape `apply` relies on: ids where it looks for
 * them, and lists where it adds to them. What it never reads is the app's
 * to draw.
 */
function isChat(value: unknown): value is Chat {
  if (!hasId(value) || !isArrayOf(value.questions, hasId) || !Array.isArray(value.turns)) return false;
  return value.turns.every(
    (turn) =>
      hasId(turn) &&
      isArrayOf(turn.items, hasId) &&
      turn.items.every((item) => isRecord(item.kind) && isString(item.kind.type)),
  );
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

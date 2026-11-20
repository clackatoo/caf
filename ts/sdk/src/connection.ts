import type {
  AgentName,
  Answer,
  Chat,
  ChatId,
  ChatSummary,
  OpenRequest,
  QuestionId,
  Refused,
  TurnId,
  UserMessage,
} from "./types.ts";
import type { ChatCopy } from "./copy.ts";

/** How messages travel: one JSON text per message, both ways. */
export interface Transport {
  send(text: string): void;
  onMessage(listener: (text: string) => void): void;
  /** Called once, when the link ends for any reason. */
  onClose(listener: () => void): void;
  close(): void;
}

/** A transport over a WebSocket at `url`, once it's open. */
export function webSocket(url: string): Promise<Transport> {
  void url;
  throw new Error("todo");
}

/** Why something asked of CAF wasn't done. */
export class CafError extends Error {
  constructor(
    readonly why:
      /** The core refused it, and nothing changed. */
      | { type: "Refused"; value: Refused }
      /** The host doesn't let viewers do this. */
      | { type: "NotAllowed" }
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
 */
export class Connection {
  private constructor() {}

  /** Says hello over `transport`, and resolves once CAF welcomes it. */
  static open(transport: Transport): Promise<Connection> {
    void transport;
    throw new Error("todo");
  }

  /** The agents a chat may be opened with. */
  get agents(): readonly AgentName[] {
    throw new Error("todo");
  }

  /** The open chats, in the order they were opened. */
  get chats(): readonly ChatSummary[] {
    throw new Error("todo");
  }

  /** Calls `listener` with the open chats each time the list changes. Returns a function that stops it. */
  onChats(listener: (chats: readonly ChatSummary[]) => void): () => void {
    void listener;
    throw new Error("todo");
  }

  /** Calls `listener` once, when the link ends. */
  onClose(listener: () => void): void {
    void listener;
    throw new Error("todo");
  }

  /**
   * Follows a chat, calling `listener` with it whole each time it changes.
   * Gaps, pictures and changes of a type this SDK doesn't know are handled
   * inside: the listener only ever sees the right chat. Given `from`, a copy
   * from an earlier connection, CAF sends only what changed since, if it
   * still can.
   */
  watch(chat: ChatId, listener: (event: WatchEvent) => void, from?: ChatCopy): Watching {
    void chat;
    void listener;
    void from;
    throw new Error("todo");
  }

  /** Asks to open a chat. The host decides: it may reject with `NotAllowed`. */
  open(request: OpenRequest): Promise<ChatId> {
    void request;
    throw new Error("todo");
  }

  /** Sends a message, resolving with its turn once the core has taken it. */
  send(chat: ChatId, message: UserMessage): Promise<TurnId> {
    void chat;
    void message;
    throw new Error("todo");
  }

  /** Stops the chat's turn. */
  stop(chat: ChatId): Promise<void> {
    void chat;
    throw new Error("todo");
  }

  /** Answers a question. */
  answer(question: QuestionId, answer: Answer): Promise<void> {
    void question;
    void answer;
    throw new Error("todo");
  }

  /** Closes a chat. */
  close(chat: ChatId): Promise<void> {
    void chat;
    throw new Error("todo");
  }

  /** Ends the link. */
  disconnect(): void {
    throw new Error("todo");
  }
}

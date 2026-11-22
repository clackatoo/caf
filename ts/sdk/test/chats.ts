// Small chats to test with. Ids start at 1000, as in caf-core's tests.

import type { Change, Chat, ChatId, Item, Numbered, Question, Setting, ToolCall, Turn } from "../src/index.ts";

/**
 * Chat `number`'s id, as caf-core's tests number chats (`ChatId::from_number`):
 * the UUID whose value is the number.
 */
export function chatId(number: number): ChatId {
  return `00000000-0000-0000-0000-${number.toString(16).padStart(12, "0")}`;
}

/** The chat the tests watch. */
export const CHAT = chatId(1000);

export function words(id: number, text: string): Item {
  return { id, window: { start: 1, end: { type: "Open" } }, parent: null, kind: { type: "Words", value: text } };
}

export function thinking(id: number, text: string): Item {
  return { id, window: { start: 1, end: { type: "Open" } }, parent: null, kind: { type: "Thinking", value: text } };
}

export function tool(title: string): ToolCall {
  return {
    title,
    kind: { type: "Execute" },
    status: { type: "Running" },
    input: { command: "ls" },
    output: "",
    app: null,
  };
}

export function toolItem(id: number, title: string): Item {
  return { id, window: { start: 1, end: { type: "Open" } }, parent: null, kind: { type: "Tool", value: tool(title) } };
}

export function turn(id: number, items: Item[] = []): Turn {
  return {
    id,
    starter: { type: "User", value: { parts: [{ type: "Text", value: "hi" }] } },
    life: { type: "Running" },
    window: { start: 1, end: { type: "Open" } },
    items,
    imported: false,
    model: null,
    spent: null,
  };
}

export function question(id: number): Question {
  return {
    id,
    window: { start: 1, end: { type: "Open" } },
    kind: {
      type: "Permission",
      value: { tool: null, title: "Run ls", options: [{ id: "allow", label: "Allow", kind: { type: "AllowOnce" } }] },
    },
  };
}

/** The agent's choice of model, as Claude offers it, `current` now. */
export function model(current: string): Setting {
  const choice = (id: string, name: string) => ({ id, name, description: null, group: null });
  return {
    kind: { type: "Model" },
    name: "Model",
    form: { type: "Choices", value: [choice("default", "Default"), choice("sonnet", "Sonnet"), choice("haiku", "Haiku")] },
    current: { type: "Choice", value: current },
  };
}

export function chat(turns: Turn[] = [], questions: Question[] = []): Chat {
  return {
    id: CHAT,
    agent: "claude",
    cwd: "/home/me/project",
    state: { type: "Open", value: { session: "s-1" } },
    turns,
    usage: null,
    questions,
    settings: { offered: [], chosen: [], own: [] },
    archived: null,
    spend: { tokens: { input: 0, cache_read: 0, cache_write: 0, output: 0, thinking: null }, reported: 0, estimated: 0, unpriced: 0, turns: 0 },
  };
}

/** Two turns, the second with words, thinking and a tool. */
export function busy(): Chat {
  return chat(
    [turn(1001, [words(1002, "old")]), turn(1003, [words(1004, "Hello"), thinking(1005, "Hm"), toolItem(1006, "ls")])],
    [question(1007)],
  );
}

export function numbered(no: number, change: Change): Numbered {
  return { no, change };
}

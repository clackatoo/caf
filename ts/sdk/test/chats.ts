// Small chats to test with. Ids start at 1000, as in caf-core's tests.

import type { Change, Chat, Item, Numbered, Question, ToolCall, Turn } from "../src/index.ts";

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

export function chat(turns: Turn[] = [], questions: Question[] = []): Chat {
  return {
    id: 1000,
    agent: "claude",
    cwd: "/home/me/project",
    state: { type: "Open", value: { session: "s-1" } },
    turns,
    usage: null,
    questions,
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

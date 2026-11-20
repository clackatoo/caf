// The chat a watch draws, and a copy's chat, share their turns and items
// with the SDK's own copy: their types must refuse a change at any depth.
// tsc checks this file: a line below that compiled would fail it.

import { expect, test } from "vitest";

import { apply, ChatCopy, type Chat, type WatchEvent } from "../src/index.ts";
import { busy, numbered, turn } from "./chats.ts";

test("a copy's chat and a watch's chat are read-only all the way down", () => {
  const copy = new ChatCopy(busy(), 10);
  const chat = copy.chat;
  const draw = (event: WatchEvent) => {
    if (event.type !== "Chat") return;
    // @ts-expect-error: read-only
    event.chat.turns.push(turn(1));
    // @ts-expect-error: read-only
    event.chat.turns[0]!.items = [];
  };
  if ((0 as number) === 1) {
    // @ts-expect-error: read-only
    chat.turns.push(turn(1));
    // @ts-expect-error: read-only
    chat.turns[0]!.items = [];
    // @ts-expect-error: read-only
    chat.turns[0]!.items.pop();
    // @ts-expect-error: read-only
    chat.questions.length = 0;
    // @ts-expect-error: read-only
    chat.turns[0]!.window.end = { type: "Open" };
    // @ts-expect-error: read-only
    chat.usage = null;
  }
  void draw;
  expect(chat.turns).toHaveLength(2);
});

test("apply takes a read-only chat, and a plain one", () => {
  const copy = new ChatCopy(busy(), 10);
  const change = numbered(11, { type: "Appended", value: { item: 1004, text: "!" } }).change;
  const fromCopy = apply(copy.chat, change);
  expect("chat" in fromCopy).toBe(true);
  // A plain chat in gives a plain chat out, as before.
  const plain: Chat = busy();
  const result = apply(plain, change);
  if ("chat" in result) {
    const mine: Chat = result.chat;
    expect(mine.turns).toHaveLength(2);
  }
  // And a copy can start from a read-only chat.
  expect(new ChatCopy(copy.chat, 10).chat).toBe(copy.chat);
});

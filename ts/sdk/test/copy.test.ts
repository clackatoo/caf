import { describe, expect, test } from "vitest";

import { ChatCopy, type Change, type Numbered } from "../src/index.ts";
import { busy, numbered } from "./chats.ts";

const append = (text: string): Change => ({ type: "Appended", value: { item: 1004, text } });
const words = (copy: ChatCopy) => copy.chat.turns[1]!.items[0]!.kind;

describe("ChatCopy", () => {
  test("starts as the picture it was given", () => {
    const chat = busy();
    const copy = new ChatCopy(chat, 10);
    expect(copy.chat).toBe(chat);
    expect(copy.latest).toBe(10);
  });

  test("takes changes in order, a new chat after each", () => {
    const copy = new ChatCopy(busy(), 10);
    const before = copy.chat;
    expect(copy.take([numbered(11, append(" a")), numbered(12, append(" b"))])).toBe("ok");
    expect(copy.latest).toBe(12);
    expect(words(copy)).toStrictEqual({ type: "Words", value: "Hello a b" });
    expect(copy.chat).not.toBe(before);
  });

  test("nothing to take is fine", () => {
    const copy = new ChatCopy(busy(), 10);
    const before = copy.chat;
    expect(copy.take([])).toBe("ok");
    expect(copy.chat).toBe(before);
  });

  test("skips changes it already has", () => {
    const copy = new ChatCopy(busy(), 10);
    expect(copy.take([numbered(11, append(" a"))])).toBe("ok");
    // 10 and 11 again, overlapping, then one new one.
    expect(copy.take([numbered(10, append(" x")), numbered(11, append(" a")), numbered(12, append(" b"))])).toBe("ok");
    expect(words(copy)).toStrictEqual({ type: "Words", value: "Hello a b" });
    expect(copy.latest).toBe(12);
  });

  test("a batch it has all of changes nothing", () => {
    const copy = new ChatCopy(busy(), 10);
    const before = copy.chat;
    expect(copy.take([numbered(9, append(" x")), numbered(10, append(" y"))])).toBe("ok");
    expect(copy.chat).toBe(before);
    expect(copy.latest).toBe(10);
  });

  test("a gap before the first new change needs a picture", () => {
    const copy = new ChatCopy(busy(), 10);
    const before = copy.chat;
    expect(copy.take([numbered(12, append(" b"))])).toBe("need-picture");
    expect(copy.chat).toBe(before);
    expect(copy.latest).toBe(10);
  });

  test("a gap partway keeps what came before it", () => {
    const copy = new ChatCopy(busy(), 10);
    expect(copy.take([numbered(11, append(" a")), numbered(13, append(" c"))])).toBe("need-picture");
    expect(copy.latest).toBe(11);
    expect(words(copy)).toStrictEqual({ type: "Words", value: "Hello a" });
  });

  test("a change it has, between new ones, is skipped", () => {
    const copy = new ChatCopy(busy(), 10);
    expect(copy.take([numbered(11, append(" a")), numbered(11, append(" a"))])).toBe("ok");
    expect(copy.take([numbered(12, append(" b")), numbered(11, append(" a")), numbered(13, append(" c"))])).toBe("ok");
    expect(words(copy)).toStrictEqual({ type: "Words", value: "Hello a b c" });
    expect(copy.latest).toBe(13);
  });

  test("a mismatch keeps what came before it", () => {
    const copy = new ChatCopy(busy(), 10);
    const missing: Change = { type: "Appended", value: { item: 9, text: "x" } };
    expect(copy.take([numbered(11, append(" a")), numbered(12, missing), numbered(13, append(" c"))])).toBe(
      "need-picture",
    );
    expect(copy.latest).toBe(11);
    expect(words(copy)).toStrictEqual({ type: "Words", value: "Hello a" });
  });

  test("a change of a type it doesn't know needs a picture", () => {
    const copy = new ChatCopy(busy(), 10);
    const later = { type: "TurnRenamed", value: {} } as unknown as Change;
    expect(copy.take([numbered(11, append(" a")), numbered(12, later)])).toBe("need-picture");
    expect(copy.latest).toBe(11);
  });

  test("a change that isn't shaped as CAF sends one needs a picture, and doesn't throw", () => {
    const shapes = [
      null,
      {},
      { no: "11", change: append(" a") },
      { no: 11 },
      { no: 11, change: null },
      { no: 11, change: { type: "Appended" } },
      { no: 11, change: { type: "Appended", value: { item: 9 } } },
    ];
    for (const shape of shapes) {
      const copy = new ChatCopy(busy(), 10);
      expect(copy.take([shape as unknown as Numbered])).toBe("need-picture");
      expect(copy.latest).toBe(10);
    }
  });

  test("a new copy from a later picture, mid-stream, carries on from it", () => {
    let copy = new ChatCopy(busy(), 10);
    expect(copy.take([numbered(11, append(" a"))])).toBe("ok");
    // A picture at 20 replaces the copy, whatever it had.
    const picture = busy();
    copy = new ChatCopy(picture, 20);
    expect(copy.take([numbered(12, append(" old")), numbered(21, append("!"))])).toBe("ok");
    expect(words(copy)).toStrictEqual({ type: "Words", value: "Hello!" });
    expect(copy.latest).toBe(21);
  });
});

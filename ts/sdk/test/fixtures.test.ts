/// <reference types="node" />
// The shared conformance fixtures: caf-core writes `fixtures/apply/*.json`
// from its own record of real chats, and every SDK's ChatCopy must turn
// each picture and its changes into exactly the chats the core had, and
// refuse a change that doesn't fit with exactly the core's Mismatch. Along
// the way, no change may alter the chat before it, and every turn and item
// a change doesn't name must be the very object it was.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, test } from "vitest";

import {
  apply,
  ChatCopy,
  type Change,
  type ChangeNo,
  type Chat,
  type DeepReadonly,
  type Mismatch,
  type Numbered,
} from "../src/index.ts";

/**
 * One fixture: a chat's first picture, then each change and either the
 * chat after it or, for a change that doesn't fit, the `Mismatch` it gives.
 * A mismatch step is always the last, and never a gap.
 */
interface Fixture {
  picture: { chat: Chat; latest: ChangeNo };
  steps: ({ change: Numbered; chat: Chat } | { change: Numbered; mismatch: Mismatch })[];
}

const folder = fileURLToPath(new URL("../../../fixtures/apply/", import.meta.url));
// No folder, or no fixtures in it, fails the run: a moved folder mustn't
// pass by checking nothing.
const files = readdirSync(folder)
  .filter((name) => name.endsWith(".json"))
  .sort();

/** Freezes a value all the way down, so any change to it throws. */
function frozen<T>(value: T): T {
  if (typeof value === "object" && value !== null && !Object.isFrozen(value)) {
    for (const each of Object.values(value)) frozen(each);
    Object.freeze(value);
  }
  return value;
}

/** The turn and item a change names, if any, in the chat it's applied to. */
function named(chat: DeepReadonly<Chat>, change: Change): { turn?: number; item?: number } {
  switch (change.type) {
    case "TurnLife":
    case "TurnModel":
    case "TurnSpent":
    case "ItemAdded":
      return { turn: change.value.turn };
    case "Appended":
    case "ToolChanged":
    case "ItemEnded": {
      const item = change.value.item;
      const turn = [...chat.turns].reverse().find((each) => each.items.some((one) => one.id === item));
      return turn === undefined ? { item } : { turn: turn.id, item };
    }
    default:
      return {};
  }
}

/** Checks that `after` shares with `before` everything `change` doesn't name. */
function expectShared(before: DeepReadonly<Chat>, after: DeepReadonly<Chat>, change: Change, at: string): void {
  if (
    ![
      "TurnAdded",
      "TurnInserted",
      "TurnLife",
      "TurnModel",
      "TurnSpent",
      "ItemAdded",
      "Appended",
      "ToolChanged",
      "ItemEnded",
    ].includes(
      change.type,
    )
  ) {
    expect(after.turns, `${at}: turns`).toBe(before.turns);
  }
  if (change.type !== "QuestionAsked" && change.type !== "QuestionGone") {
    expect(after.questions, `${at}: questions`).toBe(before.questions);
  }
  if (change.type !== "Settings") expect(after.settings, `${at}: settings`).toBe(before.settings);
  const { turn, item } = named(before, change);
  const turns = new Map(before.turns.map((each) => [each.id, each]));
  for (const each of after.turns) {
    const old = turns.get(each.id);
    if (old === undefined) continue;
    if (each.id !== turn) {
      expect(each, `${at}: turn ${each.id}`).toBe(old);
      continue;
    }
    const items = new Map(old.items.map((one) => [one.id, one]));
    for (const one of each.items) {
      const was = items.get(one.id);
      if (was !== undefined && one.id !== item) expect(one, `${at}: item ${one.id}`).toBe(was);
    }
  }
}

describe("conformance fixtures", () => {
  test("there are some", () => {
    expect(files.length, `no fixtures in ${folder}`).toBeGreaterThan(0);
  });

  test.each(files)("%s", (name) => {
    const fixture = JSON.parse(readFileSync(join(folder, name), "utf8")) as Fixture;
    const copy = new ChatCopy(frozen(fixture.picture.chat), fixture.picture.latest);
    expect(copy.chat).toStrictEqual(fixture.picture.chat);
    fixture.steps.forEach((step, at) => {
      const where = `step ${at}, change ${step.change.no}`;
      const before = copy.chat;
      const kept = structuredClone(before);
      if ("mismatch" in step) {
        expect(apply(before, frozen(step.change).change), where).toStrictEqual({ mismatch: step.mismatch });
        const latest = copy.latest;
        expect(copy.take([step.change]), where).toBe("need-picture");
        expect(copy.chat, `${where}: the copy after it`).toBe(before);
        expect(copy.latest, `${where}: the copy after it`).toBe(latest);
        expect(before, `${where}: the chat before it`).toStrictEqual(kept);
        expect(at, `${where}: a mismatch is the last step`).toBe(fixture.steps.length - 1);
        return;
      }
      expect(copy.take([frozen(step.change)]), where).toBe("ok");
      expect(copy.latest, where).toBe(step.change.no);
      expect(copy.chat, where).toStrictEqual(step.chat);
      expect(before, `${where}: the chat before it`).toStrictEqual(kept);
      expectShared(before, copy.chat, step.change.change, where);
      frozen(copy.chat);
    });
  });
});

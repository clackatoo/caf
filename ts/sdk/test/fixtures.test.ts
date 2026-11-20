/// <reference types="node" />
// The shared conformance fixtures: caf-core writes `fixtures/apply/*.json`
// from its own record of real chats, and every SDK's ChatCopy must turn
// each picture and its changes into exactly the chats the core had.

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, test } from "vitest";

import { ChatCopy, type ChangeNo, type Chat, type Numbered } from "../src/index.ts";

/** One fixture: a chat's first picture, then each change and the chat after it. */
interface Fixture {
  picture: { chat: Chat; latest: ChangeNo };
  steps: { change: Numbered; chat: Chat }[];
}

const folder = fileURLToPath(new URL("../../../fixtures/apply/", import.meta.url));
const files = existsSync(folder)
  ? readdirSync(folder)
      .filter((name) => name.endsWith(".json"))
      .sort()
  : [];

describe("conformance fixtures", () => {
  if (files.length === 0) {
    test.skip(`no fixtures in ${folder} yet: caf-core's test writes them`, () => {});
    return;
  }

  test.each(files)("%s", (name) => {
    const fixture = JSON.parse(readFileSync(join(folder, name), "utf8")) as Fixture;
    const copy = new ChatCopy(fixture.picture.chat, fixture.picture.latest);
    expect(copy.chat).toStrictEqual(fixture.picture.chat);
    fixture.steps.forEach((step, at) => {
      const taken = copy.take([step.change]);
      expect(taken, `step ${at}, change ${step.change.no}`).toBe("ok");
      expect(copy.latest, `step ${at}`).toBe(step.change.no);
      expect(copy.chat, `step ${at}, change ${step.change.no}`).toStrictEqual(step.chat);
    });
  });
});

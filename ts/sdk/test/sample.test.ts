/// <reference types="node" />
// Version 1 of the viewer wire, as caf-core pins it in
// `samples/viewer/v1.json`: one of every message, and of every variant in
// them. This SDK must read what CAF sends there, and send what CAF reads.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, test } from "vitest";

import { Connection, type FromViewer, type ToViewer } from "../src/index.ts";
import { FakeTransport } from "./transport.ts";

const file = fileURLToPath(new URL("../../../samples/viewer/v1.json", import.meta.url));
const sample = JSON.parse(readFileSync(file, "utf8")) as {
  version: number;
  from_viewer: FromViewer[];
  to_viewer: ToViewer[];
};
const toViewer = <T extends ToViewer["type"]>(type: T) =>
  sample.to_viewer.filter((message): message is Extract<ToViewer, { type: T }> => message.type === type);

async function opened(): Promise<{ caf: Connection; link: FakeTransport }> {
  const link = new FakeTransport();
  const opening = Connection.open(link);
  link.deliver(toViewer("Welcome")[0]!);
  link.deliver(toViewer("Agents")[0]!);
  const caf = await opening;
  link.take();
  return { caf, link };
}

describe("version 1's sample", () => {
  test("its agents, in every install and sign-in state, are read as they are", async () => {
    const { caf, link } = await opened();
    expect(caf.agents).toStrictEqual(toViewer("Agents")[0]!.value);
    for (const message of toViewer("Agents")) link.deliver(message);
    expect(link.closed).toBe(false);
  });

  test("every agent action is sent as CAF reads it", async () => {
    const { caf, link } = await opened();
    const expected = sample.from_viewer.filter((message) => message.type === "Agent");
    expect(expected.map((message) => message.value.action.type)).toStrictEqual([
      "Install",
      "StopInstall",
      "SignIn",
      "Paste",
      "StopSignIn",
      "SignOut",
    ]);
    for (const { value } of expected) {
      const { agent, action } = value;
      switch (action.type) {
        case "Install":
          void caf.install(agent);
          break;
        case "StopInstall":
          void caf.stopInstall(agent);
          break;
        case "SignIn":
          void caf.signIn(agent);
          break;
        case "Paste":
          void caf.paste(agent, action.value);
          break;
        case "StopSignIn":
          void caf.stopSignIn(agent);
          break;
        case "SignOut":
          void caf.signOut(agent);
          break;
      }
    }
    // The same, but for the request numbers, which are the viewer's own.
    const numbered = (messages: FromViewer[]) =>
      messages.map((message) => (message.type === "Agent" ? { ...message.value, request: 0 } : message));
    expect(numbered(link.take())).toStrictEqual(numbered(expected));
  });
});

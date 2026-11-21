/// <reference types="node" />
// Version 1 of the viewer wire, as caf-core pins it in
// `samples/viewer/v1.json`: one of every message, and of every variant in
// them. This SDK must read what CAF sends there, and send what CAF reads.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, test } from "vitest";

import { CafError, Connection, type FromViewer, type ToViewer, type WatchEvent } from "../src/index.ts";
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

  test("its chats are read, by their ids", async () => {
    const { caf, link } = await opened();
    const [picture] = toViewer("Picture");
    const id = picture!.value.chat.id;
    expect(caf.chats.map((chat) => chat.id)).toContain(id);
    const events: WatchEvent["type"][] = [];
    const watch = caf.watch(id, (event) => events.push(event.type));
    link.deliver(picture!);
    for (const message of toViewer("Changes")) link.deliver(message);
    for (const message of toViewer("Chats")) link.deliver(message);
    expect(watch.copy?.latest).toBe(toViewer("Changes").at(-1)!.value.changes.at(-1)!.no);
    for (const message of toViewer("Ended")) link.deliver(message);
    expect(events.at(-1)).toBe("Ended");
    expect(link.closed).toBe(false);
  });

  test("its saved chats, in every state, are read as they are", async () => {
    const { caf, link } = await opened();
    const [saved] = toViewer("Saved");
    expect(saved!.value.map((chat) => chat.state.type)).toStrictEqual([
      "Saved",
      "Restoring",
      "Loaded",
      ...saved!.value.slice(3).map(() => "Failed"),
    ]);
    link.deliver(saved!);
    expect(link.closed).toBe(false);
    expect(caf.saved).toStrictEqual(saved!.value);
  });

  test("restoring is sent as CAF reads it", async () => {
    const { caf, link } = await opened();
    const expected = sample.from_viewer.filter((message) => message.type === "Restore");
    expect(expected).toHaveLength(1);
    for (const { value } of expected) void caf.restore(value.chat, value.shelf).catch(() => {});
    const numbered = (messages: FromViewer[]) =>
      messages.map((message) => (message.type === "Restore" ? { ...message.value, request: 0 } : message));
    expect(numbered(link.take())).toStrictEqual(numbered(expected));
  });

  test("every outcome is read: what was asked for resolves with it, the rest reject saying why", async () => {
    const { caf, link } = await opened();
    const [picture] = toViewer("Picture");
    const chat = picture!.value.chat.id;
    for (const { value } of toViewer("Done")) {
      const { outcome } = value;
      const asked =
        outcome.type === "Opened"
          ? caf.restore(chat, "work")
          : outcome.type === "Sent"
            ? caf.send(chat, { parts: [] })
            : caf.stop(chat);
      const [request] = link.take();
      link.deliver({ type: "Done", value: { ...value, request: (request!.value as { request: number }).request } });
      const settled = await asked.then(
        (ok: unknown) => ({ ok }),
        (error: unknown) => ({ error }),
      );
      if ("ok" in settled) {
        expect(outcome.type).toBeOneOf(["Opened", "Sent", "Done"]);
        expect(settled.ok).toStrictEqual("value" in outcome ? outcome.value : undefined);
      } else {
        expect(settled.error).toBeInstanceOf(CafError);
        expect((settled.error as CafError).why).toStrictEqual(outcome);
      }
    }
    expect(link.closed).toBe(false);
  });
});

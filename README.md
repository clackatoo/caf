# CAF

The Clackatoo Agent Framework: a Rust library for building robust,
feature-rich chats with coding agents such as Claude Code and Codex,
embedded in your own program.

CAF speaks the [Agent Client Protocol](https://agentclientprotocol.com)
to the agent, absorbs each agent's quirks, and keeps its own record of
every conversation. Your program describes what it can show, and draws
the chat from CAF's record. CAF itself draws nothing.

It grows out of the agent backend of
[clackatoo](https://github.com/clackatoo/clackatoo), and clackatoo is its
first user.

## What's here

- `crates/core` (`caf-core`): the state and rules of every chat, with no
  I/O at all.
- `crates/driver-tokio` (`caf-driver-tokio`): CAF on tokio, including
  `viewer::serve` for a browser or other viewer.
- `crates/save` (`caf-save`): how chats are saved, so they carry on from
  one run to the next: the format, which stays readable forever, and a
  store that keeps chats in files.
- `ts/sdk` (`@clackatoo/caf`): the TypeScript caf-sdk.
- `crates/debugger` and `ts/debugger` (`caf-debugger`): a reference chat
  view beside an inspector of every step CAF takes, with saved chats,
  recordings and time travel.
- `crates/fake-agent`: a scriptable fake adapter for tests.

CAF is built a feature at a time, each feature's API designed in full
before it's built. Its docs are on
[Learn](https://learn.peios.org).

## License

MIT or Apache-2.0, at your option.

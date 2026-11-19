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

> **CAF is being designed.** There's no code yet. Each feature's API is
> designed in full before it's built.

## License

MIT or Apache-2.0, at your option.

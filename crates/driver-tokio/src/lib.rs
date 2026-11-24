//! CAF on tokio: drivers for [`caf_core`], and the wiring that runs them.
//!
//! Most apps want only [`Caf`]: start it with a [`Config`], open chats,
//! send messages, and watch them change.
//!
//! ```ignore
//! let claude = AgentSpec::installed("claude", Builtin::ClaudeAgentAcp);
//! let (caf, running) = Caf::<NoAppItems>::start(Config::new(Host::new("eyespy", "0.4.0"), vec![claude]));
//! let chat = caf.open_chat(OpenChat::new("claude", "/home/me/project")).await?;
//! let mut watch = chat.watch();
//! chat.send(UserMessage::text("Fix the tests")).await?;
//! while let Some(update) = watch.next().await {
//!     match update {
//!         Update::Picture(chat) => draw_all(&chat),
//!         Update::Changes(changes) => changes.iter().for_each(draw_change),
//!         _ => {}
//!     }
//! }
//! caf.shutdown().await;
//! running.await;
//! ```
//!
//! # Installing and signing in
//!
//! An agent offered with [`AgentSpec::installed`] is installed by CAF
//! itself, at the version its profile was checked at, in a folder every
//! CAF program on the machine shares ([`Config::installs`], by default
//! `~/.local/share/caf/agents`). CAF looks at what's installed as it
//! starts. A chat opened with an agent that isn't installed yet waits for
//! it, and the user decides: installing Claude's adapter takes about
//! 630 MB.
//!
//! ```ignore
//! let claude = AgentName("claude".into());
//! let mut agents = caf.watch_agents();
//! caf.install(&claude).await?; // progress shows in `agents`
//! caf.sign_in(&claude).await?; // so does the link to open
//! while let Some(statuses) = agents.next().await {
//!     if let SignInState::SigningIn(link) = &statuses[0].sign_in {
//!         show_link(link); // the user opens it, and may paste a code back
//!     }
//! }
//! caf.paste(&claude, code_the_user_pasted).await?;
//! ```
//!
//! - [`Caf::watch_agents`] follows where every agent stands, installed or
//!   not and signed in or not ([`AgentStatus`]).
//! - [`Caf::install`] and [`Caf::stop_install`]; chats waiting for the
//!   agent start once it's installed, or are told why it wasn't.
//! - [`Caf::sign_in`], [`Caf::paste`], [`Caf::stop_sign_in`] and
//!   [`Caf::sign_out`] run the sign-in the agent's profile names, never one
//!   the agent suggests. The link it gives is checked against the sites
//!   its profile allows before anyone sees it.
//! - [`Caf::check_agents`] looks again, such as after the user installed
//!   Node.
//!
//! The work is the [`SetupDriver`]'s: npm installs from a lockfile CAF
//! ships, checked downloads, and the agent's own sign-in commands. It needs
//! Node 22 or later and npm on `PATH` for agents installed with npm, which
//! Claude's is. CAF keeps how an agent is signed in, never who: no email,
//! and no pasted code, reaches its state or a recording.
//!
//! [`AgentSpec::installed`]: caf_core::agent::AgentSpec::installed
//! [`AgentStatus`]: caf_core::agent::AgentStatus
//!
//! # Saving chats
//!
//! Give CAF a store ([`Config::store`]), such as caf-save's `FileStore`,
//! and a chat opened with a shelf ([`OpenChat::shelf`]) is saved as it
//! goes, and comes back in a later run:
//!
//! ```ignore
//! config.store = Some(Box::new(FileStore::new(FileStore::default_root("eyespy").unwrap())));
//! let (caf, running) = Caf::<NoAppItems>::start(config);
//! let mut saved = caf.watch_saved();
//! caf.list_saved(Shelf("project-x".into())).await?;
//! let chats = saved.next().await; // each with its header and state
//! let chat = caf.restore(RestoreChat::new(id, Shelf("project-x".into()))).await?;
//! ```
//!
//! - [`Caf::restore`] returns once the chat is loaded, idle, or archived
//!   if it was closed: its agent starts, carrying on its session, when a
//!   message is sent.
//! - [`ChatHandle::put_away`] saves a chat whole and lets it go from
//!   memory; closing a saved chat does too. [`Caf::delete`] deletes one
//!   for good, and [`Caf::move_shelf`] moves a shelf's chats.
//! - Closing a chat archives it, and it's restored archived, until
//!   [`ChatHandle::unarchive`] or a message sent to it unarchives it.
//! - [`ChatHandle::set_data`] keeps the app's own data on a chat, saved
//!   with it, and its label shows in [`Caf::watch_chats`].
//! - The store runs on a thread of its own, and [`Caf::shutdown`] returns
//!   once every saved chat is written whole and let go.
//!
//! [`OpenChat::shelf`]: caf_core::OpenChat::shelf
//!
//! # Usage
//!
//! Each turn records what it spent, if the agent says, and each chat adds
//! its turns up ([`Chat::spend`]). Every turn also goes into a usage ledger
//! every CAF app on the machine shares ([`Config::usage`], on by default),
//! so a program can show how much the user has used across all of them:
//!
//! ```ignore
//! let summary = caf.usage(UsageQuery::new(since, until, "Europe/London")).await?;
//! caf.set_price("claude-haiku-5-5".into(), Some(ModelPrice::new(1.0, 5.0))).await?;
//! ```
//!
//! - The ledger is written on a thread of its own, and never holds a chat
//!   up. [`Caf::usage`] reads a summary on another.
//! - Cost the agent didn't report is estimated from [`Caf::prices`]: the
//!   user's own ([`Caf::set_price`]), then LiteLLM's table, fetched once a
//!   day unless [`UsageConfig::fetch_prices`] is off, then a snapshot
//!   bundled with CAF.
//! - `config.usage = None` keeps no ledger: chats still record their
//!   spend. Tests should set it, or a folder of their own.
//!
//! [`Chat::spend`]: caf_core::Chat::spend
//!
//! # Serving CAF's tools
//!
//! CAF can give agents tools of its own (Read, Edit, Write and Bash), which
//! it runs itself, asking the user as the [`Policy`] says. The agent
//! starts them as an MCP server: a relay, which connects back to CAF over
//! a unix socket. The relay is usually the app's own program, so the app
//! calls [`caf_relay::relay_if_asked`] first thing in its `main`, and
//! starts CAF serving through it:
//!
//! ```ignore
//! fn main() {
//!     caf_relay::relay_if_asked();
//!     // ...
//! }
//! config.tools.relay = Relay::ThisProgram;
//! let chat = caf.open_chat(OpenChat::new("claude", folder).with_tools(Tools::caf(CafTool::ALL))).await?;
//! ```
//!
//! - [`ToolsConfig::relay`] is [`Relay::None`] by default: nothing is
//!   served, and a chat given tools is refused. With a relay,
//!   [`Caf::try_start`] checks it answers as `relay_if_asked` does, and
//!   says if it doesn't ([`StartError`]); [`Caf::start`] panics with
//!   that.
//! - The socket is in a folder only the user can reach
//!   ([`ToolsConfig::folder`]), and each chat's relay proves which chat
//!   it's for with a token made from a key drawn as CAF starts. The key is
//!   never in a recording.
//! - [`ToolsConfig::runner`] runs the calls. Until CAF's tools are built,
//!   it's [`NotBuilt`], which fails every call saying so.
//! - Unix only, for now.
//!
//! [`OpenChat`]: caf_core::OpenChat
//!
//! # Serving viewers
//!
//! A viewer is anything that shows chats from outside the host's own
//! code: a web page through the TypeScript caf-sdk, another process, a
//! debugger. [`viewer::serve`] runs CAF's side of the viewer wire
//! ([`caf_core::viewer`]) for one of them, over two channels the host
//! fills from its own transport, such as a WebSocket:
//!
//! ```ignore
//! let rules = Arc::new(ViewerRules::default());
//! // for each viewer that connects:
//! let (to_caf, from) = mpsc::channel(16);
//! let (to, mut to_viewer) = mpsc::channel(64);
//! tokio::spawn(serve(caf.clone(), Arc::clone(&rules), from, to));
//! // decode what the viewer sends onto `to_caf`; encode and send it
//! // what comes out of `to_viewer`, until either ends.
//! ```
//!
//! - Viewers see every chat in this [`Caf`]. A host whose users mustn't
//!   see each other's chats starts a Caf for each.
//! - [`ViewerRules`](viewer::ViewerRules) say what they may do. Opening
//!   is the host's decision: by default viewers can't open chats, only
//!   watch and act on the host's. `act: false` makes them watch only.
//! - A slow viewer costs little to keep: while its channel is full, its
//!   chats' changes wait in the core's bounded list, and once it falls
//!   further behind than that it gets each chat whole again. What waits
//!   for it is what's in its channel, each message up to a whole chat.
//! - A viewer watches at most
//!   [`max_watches`](viewer::ViewerRules::max_watches) chats at once.
//! - Every viewer hears every agent's status, after the welcome and each
//!   time it changes. Installing and signing in from a viewer is the
//!   host's decision too: [`agents`](viewer::ViewerRules::agents) is off
//!   by default.
//! - Every viewer hears the saved chats on every shelf the host has
//!   listed, and may restore one if [`restore`](viewer::ViewerRules::restore)
//!   allows it, which by default it doesn't.
//! - A viewer doesn't keep CAF running, and `serve` ends when CAF does,
//!   after telling the viewer each watched chat has ended.
//!
//! [`Caf::agents`], [`Caf::run`] and [`Caf::watch_chats`] give what a
//! host needs to serve viewers some other way, and [`ChatHandle::watch_from`] resumes a
//! viewer's watch from the last change it has.
//!
//! # Watching every step
//!
//! [`Caf::start_watching`] gives a [`Steps`] every event the core takes
//! and every directive it gives out, exactly: for debuggers, and for
//! tests that check CAF against a replay. Unlike a recording, they hold
//! the host's secrets, so they're for the host's eyes only.
//!
//! The drivers inside are separate pieces, each usable on its own by a
//! host that wires the core up itself: [`AgentDriver`] runs adapter
//! processes, [`TimerDriver`] wakes the core, [`SetupDriver`] installs
//! agents and signs in to them, and a [`Policy`] answers permission
//! requests. The storage driver's work is `caf_save::carry_out`, which
//! does one directive to a store, on any thread, with no runtime: the kit
//! runs it on a thread of its own.

mod agent;
mod caf;
mod policy;
mod setup;
mod steps;
mod storage;
mod timer;
mod tools;
mod usage;
pub mod viewer;

pub use agent::{AgentDriver, AgentLimits};
pub use caf::{
    AgentsWatch, Caf, ChatHandle, ChatsWatch, Config, Error, Running, SavedWatch, Update,
    UsageConfig, Watch,
};
pub use policy::{AlwaysAsk, Policy, RefuseAll};
pub use setup::SetupDriver;
pub use steps::{Step, Steps};
pub use timer::TimerDriver;
pub use tools::{Call, NotBuilt, Preview, Ran, Relay, Runner, StartError, ToolsConfig};

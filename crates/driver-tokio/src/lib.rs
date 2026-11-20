//! CAF on tokio: drivers for [`caf_core`], and the wiring that runs them.
//!
//! Most apps want only [`Caf`]: start it with a [`Config`], open chats,
//! send messages, and watch them change.
//!
//! ```ignore
//! let claude = AgentSpec::new(
//!     "claude",
//!     SpawnCommand::new("/home/me/.local/share/caf/agents/claude-0.98.0/bin/claude-agent-acp"),
//!     ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
//! );
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
//! processes, [`TimerDriver`] wakes the core, and a [`Policy`] answers
//! permission requests.

mod agent;
mod caf;
mod policy;
mod steps;
mod timer;
pub mod viewer;

pub use agent::{AgentDriver, AgentLimits};
pub use caf::{Caf, ChatHandle, ChatsWatch, Config, Error, Running, Update, Watch};
pub use policy::{AlwaysAsk, Policy, RefuseAll};
pub use steps::{Step, Steps};
pub use timer::TimerDriver;

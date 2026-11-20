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

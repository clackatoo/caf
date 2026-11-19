//! CAF on tokio: drivers for [`caf_core`], and the wiring that runs them.
//!
//! Most apps want only [`Caf`]: start it with a [`Config`], open chats,
//! send messages, and watch them change.
//!
//! ```ignore
//! let (caf, running) = Caf::<NoAppItems>::start(Config::new(vec![claude]));
//! let chat = caf.open_chat(OpenChat::new("claude", "/home/me/project")).await?;
//! let mut watch = chat.watch();
//! chat.send(UserMessage::text("Fix the tests")).await?;
//! while let Some(update) = watch.next().await {
//!     match update {
//!         Update::Picture(chat) => draw_all(&chat),
//!         Update::Changes(changes) => changes.iter().for_each(draw_change),
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

// The API draft for review: bodies are `todo!()` until it is approved.
#![allow(unused_variables, dead_code)]

mod agent;
mod caf;
mod policy;
mod timer;

pub use agent::{AgentDriver, AgentLimits};
pub use caf::{Caf, ChatHandle, Config, Gone, Running, Update, Watch};
pub use policy::{AlwaysAsk, Policy, RefuseAll};
pub use timer::TimerDriver;

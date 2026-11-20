//! Serving viewers: CAF's side of the viewer wire
//! ([`caf_core::viewer`]), for a viewer at the other end of a WebSocket,
//! a socket or a pipe.
//!
//! The host owns the transport: it reads the viewer's messages, decodes
//! them (JSON for a web page, MessagePack in named mode for a Rust
//! viewer), and hands them to [`serve`] on one channel; it takes
//! [`ToViewer`]s from the other, encodes them and sends them on.
//!
//! ```ignore
//! let rules = Arc::new(ViewerRules::default()); // viewers may not open chats
//! let (to_caf, from) = mpsc::channel(16);
//! let (to, mut to_viewer) = mpsc::channel(64);
//! tokio::spawn(serve(caf.clone(), rules, from, to));
//! // …then pass each decoded message to `to_caf`, and send on each one
//! // `to_viewer` gives, encoded.
//! ```

use std::sync::Arc;

use caf_core::OpenChat;
use caf_core::chat::AppItem;
use caf_core::viewer::{FromViewer, OpenRequest, ToViewer};
use tokio::sync::mpsc;

use crate::caf::Caf;

/// Turns a viewer's request to open a chat into one, or refuses it: see
/// [`ViewerRules::open`].
pub type Opener = dyn Fn(&OpenRequest) -> Option<OpenChat> + Send + Sync;

/// What viewers may do. One Caf is one circle of trust: a viewer may
/// watch any chat in it. A host whose users mustn't see each other's
/// chats runs a Caf for each.
#[non_exhaustive]
pub struct ViewerRules {
    /// Turns a viewer's request to open a chat into one, or `None` to
    /// refuse it with [`Outcome::NotAllowed`]. It's the host's to decide:
    /// it chooses the agent's tools, and may limit the folders. The
    /// default refuses every request, so viewers can only watch chats the
    /// host opened.
    ///
    /// [`Outcome::NotAllowed`]: caf_core::viewer::Outcome::NotAllowed
    pub open: Box<Opener>,
    /// Whether viewers may send, stop, answer and close. `true` by
    /// default; `false` makes them watch only.
    pub act: bool,
}

impl Default for ViewerRules {
    fn default() -> Self {
        Self {
            open: Box::new(|_| None),
            act: true,
        }
    }
}

/// Runs the viewer wire for one viewer, until the viewer's side closes
/// `from` or drops `to`'s receiver, the viewer breaks the wire's rules
/// (such as not starting with `Hello`), or CAF shuts down. CAF going on
/// without this viewer is unaffected either way.
///
/// `to` should be bounded. While it's full, no more of a chat's changes
/// are taken; once there's room, a viewer that fell far behind gets the
/// chat whole again rather than every change it missed. So a slow viewer
/// never makes CAF hold more for it than one chat's latest picture.
pub async fn serve<A: AppItem>(
    caf: Caf<A>,
    rules: Arc<ViewerRules>,
    from: mpsc::Receiver<FromViewer>,
    to: mpsc::Sender<ToViewer<A>>,
) {
    let _ = (caf, rules, from, to);
    todo!()
}

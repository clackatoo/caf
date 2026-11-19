//! The policy driver: who answers "may the agent do this?".
//!
//! The kit asks a [`Policy`] for every permission request, and carries on
//! while it decides. CAF's Permission Engine will be one (*later*);
//! apps can write their own, such as eyespy's fixed list.

use std::future::Future;
use std::pin::Pin;

use caf_core::ChatId;
use caf_core::directive::PermissionRequest;
use caf_core::event::PolicyAnswer;

/// Decides permission requests.
pub trait Policy: Send + 'static {
    /// Decides one request. It may take as long as it needs: the core uses
    /// the answer only if the request still stands by then.
    ///
    /// Return at once, and do the work in the future: `decide` itself runs
    /// on CAF's task, so a slow one holds up every chat. The future runs
    /// as a task of its own, and is dropped if the request is withdrawn,
    /// such as when the user stops the turn. A panic, in `decide` or in
    /// its future, refuses the request.
    fn decide(
        &mut self,
        chat: ChatId,
        request: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>>;
}

/// Asks the user about everything. The default.
#[derive(Clone, Copy, Debug, Default)]
pub struct AlwaysAsk;

impl Policy for AlwaysAsk {
    fn decide(
        &mut self,
        _: ChatId,
        _: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        Box::pin(std::future::ready(PolicyAnswer::AskUser))
    }
}

/// Refuses everything: for hosts whose agents need no permission at all.
#[derive(Clone, Copy, Debug, Default)]
pub struct RefuseAll;

impl Policy for RefuseAll {
    fn decide(
        &mut self,
        _: ChatId,
        _: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        Box::pin(std::future::ready(PolicyAnswer::Refuse))
    }
}

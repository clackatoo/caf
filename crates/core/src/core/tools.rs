//! Calls to the tools CAF serves: joining each to the agent's report of
//! it, asking about it, and recording what it did.
//!
//! A call goes: [`ToolEvent::Called`] makes or joins its item, Pending,
//! with its action and title; an edit is previewed first
//! ([`ToolDirective::Preview`](crate::directive::ToolDirective::Preview)),
//! so its diff is on the call before anyone's asked; then the policy is
//! asked, and the user if it says so (Asking); then it's run (Running), or
//! refused (Refused). Its output comes in as it runs, and it ends Done or
//! Failed, or Cancelled if its turn is stopped or the agent cancels it.

use super::Core;
use crate::chat::{AppData, AppItem};
use crate::directive::Directive;
use crate::event::ToolEvent;
use crate::ids::CallId;
use crate::time::Now;

impl<A: AppItem, D: AppData> Core<A, D> {
    /// The tools driver reported something of a call.
    pub(super) fn tool_event(
        &mut self,
        now: Now,
        call: CallId,
        event: ToolEvent,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let _ = (now, call, event, out);
        todo!()
    }
}

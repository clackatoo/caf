//! Calls to the tools CAF serves: joining each to the agent's report of
//! it, asking about it, and recording what it did.
//!
//! A call goes: [`ToolEvent::Called`] makes or joins its item, Pending,
//! with its action and title; an edit is previewed first
//! ([`ToolDirective::Preview`]), so its diff is on the call before
//! anyone's asked; then the policy is asked, and the user if it says so
//! (Asking); then it's run (Running), or refused (Refused). Its output
//! comes in as it runs, and it ends Done or Failed, or Cancelled if its
//! turn is stopped or the agent cancels it.
//!
//! Every call the driver reports is answered exactly once, with one of
//! [`ToolDirective::Run`], [`ToolDirective::Refuse`] or
//! [`ToolDirective::Cancel`], and nothing after: unless the agent gave up
//! on it first ([`ToolEvent::Gone`]), when there's no one to answer.
//! Anything the driver reports of a call the core has finished with is
//! ignored.
//!
//! The agent's report of a call (ACP's `tool_call`) and the call itself
//! may come in either order; see [`crate::tool`] for how they're joined.
//! Once joined, CAF's record is the truth: the agent's reports of the
//! call change nothing of it ([`Core::served_report`]).

use std::collections::VecDeque;

use serde::Serialize;
use serde_json::{Value, json};

use super::Refused;
use super::clean::agent_text;
use super::sessions::mcp_server;
use super::state::Open;
use super::{Core, report};
use crate::agent::{EnvChange, McpServer};
use crate::chat::{AppData, AppItem, ItemKind, ToolCall, ToolStatus};
use crate::chat::{ChatState, Note};
use crate::directive::{
    Directive, Level, PermissionRequest, PolicyDirective, RefusedCall, ToolDirective,
};
use crate::event::RelayChange;
use crate::event::{PolicyAnswer, ToolEvent};
use crate::ids::{CallId, ChatId, ItemId, QuestionId, Ticket, TurnId};
use crate::profiles::{CAF_SERVER, served_instructions};
use crate::question::{OptionId, OptionKind, Permission, PermissionOption, QuestionKind};
use crate::time::{Now, Window};
use crate::tool::{
    Action, CafTool, DecidedBy, Decision, Ending, Lines, MOST_LINE, MOST_OUTPUT, Origin, Output,
    Piece, StoppedBy, output_size,
};

/// The longest title CAF writes for a call, in bytes.
const MOST_TITLE: usize = 1024;

/// The id and label of CAF's own options when it asks the user about a
/// call: allow it once, or refuse it.
const ALLOW: (&str, &str) = ("allow", "Allow");
const REFUSE: (&str, &str) = ("refuse", "Refuse");

/// A call to a tool CAF serves, not finished yet.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Call {
    /// The chat it came in for.
    pub(super) chat: ChatId,
    /// Its item.
    pub(super) item: ItemId,
    /// What it does.
    pub(super) action: Action,
    /// Where it is.
    pub(super) stage: Stage,
    /// What of its output is kept, if it's running a command.
    pub(super) printed: Printed,
}

/// Where a call CAF serves is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Stage {
    /// Its preview is being worked out.
    Previewing,
    /// The policy is deciding, under this ticket.
    Deciding(Ticket),
    /// The user is asked, by this question.
    Asking(QuestionId),
    /// Running.
    Running,
}

/// Which of the lines a command printed are kept, as the driver numbers
/// them: the kept lines are `fixed` lines no longer rewritten, then the
/// pieces of the driver's lines from `first` on, `pieces[i]` of line
/// `first + i`, the first of them perhaps missing some from its start.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Printed {
    /// How many kept lines come before the driver's line `first`: those
    /// kept from before lines it skipped.
    pub(super) fixed: u64,
    /// The driver's number of the first line any of which is kept.
    pub(super) first: u64,
    /// How many kept lines each of the driver's lines from there is.
    pub(super) pieces: VecDeque<u64>,
}

/// Work owed to a driver, found where there's no list of work to add it
/// to, such as a turn ending: added when the call into the core finishes.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Owed {
    /// Cancel a call CAF serves.
    Cancel(CallId),
    /// Withdraw a request from the policy.
    Withdrawn(Ticket),
    /// Stop serving a chat.
    Unserve(ChatId),
}

impl Owed {
    /// The work, as a directive.
    pub(super) fn directive<A: AppItem, D: AppData>(self) -> Directive<A, D> {
        match self {
            Self::Cancel(call) => Directive::Tool(ToolDirective::Cancel { call }),
            Self::Withdrawn(ticket) => Directive::Policy(PolicyDirective::Withdrawn { ticket }),
            Self::Unserve(chat) => Directive::Tool(ToolDirective::Unserve { chat }),
        }
    }
}

/// Whether the agent's report of a call CAF serves has joined it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Joined {
    /// Not yet.
    No,
    /// The report with this id, which said nothing yet of the call's
    /// input: it's let go if its input turns out to be another's.
    Maybe(String),
    /// Yes.
    Yes,
}

/// A tool call item CAF serves, in its chat's run: whatever the agent
/// reports of it changes nothing.
#[derive(Clone, Debug, Serialize)]
pub(super) struct Served {
    /// The tool.
    pub(super) tool: CafTool,
    /// Whether the agent's report of it has joined it.
    pub(super) report: Joined,
    /// The agent gave its id for the call with it, so its report joins
    /// by that id, never by input.
    pub(super) keyed: bool,
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// The tools driver said how CAF's tools are served.
    pub(super) fn serving_given(
        &mut self,
        serving: crate::event::Serving,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if self.serving.is_some() {
            report(
                out,
                Level::Warn,
                "the tools driver said twice how CAF's tools are served: the first stands"
                    .to_owned(),
            );
            return;
        }
        self.serving = Some(serving);
    }

    /// A chat's connection to CAF's tools changed. Lost while its session
    /// is open, the chat says so, once a session: not while it's closing,
    /// nor once its session has gone.
    pub(super) fn relay_changed(
        &mut self,
        now: Now,
        chat: ChatId,
        change: RelayChange,
        out: &mut Vec<Directive<A, D>>,
    ) {
        report(
            out,
            Level::Debug,
            format!("chat {}'s connection to CAF's tools: {change:?}", chat.0),
        );
        if change != RelayChange::Lost {
            return;
        }
        let open = matches!(
            self.chats.get(&chat).map(|whole| &whole.state),
            Some(ChatState::Open { .. })
        );
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        if !open
            || !matches!(run.open, Open::Live { .. })
            || run.tools_lost
            || run.caf_tools.is_empty()
        {
            return;
        }
        run.tools_lost = true;
        self.say(now, chat, Note::ToolsLost);
    }

    /// The MCP server `caf`, as it's sent, serving `tools` to `chat`:
    /// `None` if there are none. [`Refused::ToolsNotServed`] if nothing
    /// serves them, and [`Refused::Unsendable`] if the relay's command
    /// can't be sent exactly as it is.
    pub(super) fn caf_server(
        &self,
        chat: ChatId,
        tools: &[CafTool],
    ) -> Result<Option<Value>, Refused> {
        if tools.is_empty() {
            return Ok(None);
        }
        let serving = self.serving.as_ref().ok_or(Refused::ToolsNotServed)?;
        let mut command = serving.relay.clone();
        let set = |name: &str, value: std::ffi::OsString| EnvChange::Set(name.into(), value);
        command.env.extend([
            set("CAF_RELAY", serving.socket.clone().into_os_string()),
            set("CAF_RELAY_CHAT", chat.to_string().into()),
            set(
                "CAF_RELAY_TOKEN",
                crate::mcp::token(&serving.key, chat).into(),
            ),
        ]);
        let server = McpServer::Stdio {
            name: CAF_SERVER.to_owned(),
            command,
        };
        mcp_server(&server).map(Some).ok_or(Refused::Unsendable)
    }

    /// Tells the tools driver what to serve `chat`, if it has tools.
    pub(super) fn serve(&self, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let Some(run) = self.runs.get(&chat).filter(|run| !run.caf_tools.is_empty()) else {
            return;
        };
        out.push(Directive::Tool(ToolDirective::Serve {
            chat,
            tools: run.caf_tools.clone(),
            instructions: served_instructions(&self.host.name, &run.caf_tools),
        }));
    }

    /// Tells the tools driver, as this call into the core finishes, to stop
    /// serving `chat`, if it has tools: it's let go.
    pub(super) fn unserve(&mut self, chat: ChatId) {
        if self
            .runs
            .get(&chat)
            .is_some_and(|run| !run.caf_tools.is_empty())
        {
            self.owed.push(Owed::Unserve(chat));
        }
    }

    /// The tools driver reported something of a call.
    pub(super) fn tool_event(
        &mut self,
        now: Now,
        call: CallId,
        event: ToolEvent,
        out: &mut Vec<Directive<A, D>>,
    ) {
        match event {
            ToolEvent::Called {
                chat,
                tool,
                input,
                agent_call,
            } => self.called(now, call, chat, tool, input, agent_call, out),
            ToolEvent::Previewed { pieces } => self.previewed(call, pieces, out),
            ToolEvent::Output { from, lines } => self.printed(call, from, lines, out),
            ToolEvent::Finished { done, pieces } => self.finished(now, call, done, pieces, out),
            ToolEvent::Gone => self.gone(now, call, out),
        }
    }

    /// The agent called one of CAF's tools: refused, or joined to its
    /// report, or made an item, and then previewed or asked about.
    #[allow(clippy::too_many_arguments)] // the call, as the driver gives it
    fn called(
        &mut self,
        now: Now,
        call: CallId,
        chat: ChatId,
        tool: CafTool,
        input: Value,
        agent_call: Option<String>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if self.calls.contains_key(&call) {
            return report(
                out,
                Level::Warn,
                format!("ignored call {} reported twice", call.0),
            );
        }
        let refuse = |out: &mut Vec<Directive<A, D>>, why| {
            out.push(Directive::Tool(ToolDirective::Refuse { call, why }));
        };
        // A turn being stopped takes no new calls.
        let turn = self.runs.get(&chat).and_then(|run| match &run.running {
            Some(running) if running.stop_by.is_some() => None,
            _ => self.open_turn(chat),
        });
        let Some(turn) = turn else {
            return refuse(out, RefusedCall::NotRunning);
        };
        let Some(run) = self.runs.get(&chat) else {
            return refuse(out, RefusedCall::NotRunning);
        };
        if !run.caf_tools.contains(&tool) {
            return refuse(out, RefusedCall::NotGiven);
        }
        let cwd = run.cwd.clone();
        // A report held only for now by a call that came without the
        // agent's id is this call's, as it names it: let go.
        if let Some(id) = &agent_call
            && let Some(run) = self.runs.get_mut(&chat)
            && let Some(&item) = run.tools.get(id)
            && let Some(served) = run.served.get_mut(&item)
            && served.report == Joined::Maybe(id.clone())
        {
            served.report = Joined::No;
            run.tools.remove(id);
        }
        // The agent's id for a call names one call: a second call naming
        // it is never joined to the first's item.
        let agent_call = agent_call.filter(|id| {
            let taken = (self.runs.get(&chat))
                .and_then(|run| run.tools.get(id))
                .is_some_and(|item| self.is_served(chat, *item));
            if taken {
                report(
                    out,
                    Level::Warn,
                    format!(
                        "call {} gave the agent's id of an earlier call: not joined by it",
                        call.0
                    ),
                );
            }
            !taken
        });
        let (joins, joined) = match &agent_call {
            Some(id) => (self.reported_item(chat, id), Joined::Yes),
            None => match self.unjoined_report(chat, tool, &input) {
                Some((item, joined)) => (Some(item), joined),
                None => (None, Joined::No),
            },
        };
        let action = match action_of(tool, &input, &cwd) {
            Ok(action) => action,
            Err(why) => {
                // The agent's report of it is all there is: it failed.
                if let (Some(item), Joined::Yes) = (joins, &joined) {
                    self.fail_report(now, chat, item, tool, &why);
                }
                return refuse(out, RefusedCall::BadInput(why));
            }
        };
        let (kept, input_cut) = crate::tool::capped_input(input);
        let record = Box::new(ToolCall {
            title: title_of(&action, &cwd),
            kind: action.kind(),
            status: ToolStatus::Pending,
            origin: Origin::Caf(tool),
            action: Some(action.clone()),
            input: Some(kept),
            input_cut,
            result: Vec::new(),
            locations: locations_of(&action),
            locations_cut: 0,
            decision: Decision::Undecided,
            app: None,
            stopped_by: None,
        });
        let item = match joins {
            Some(item) => {
                self.set_tool(chat, item, record);
                item
            }
            None => self.add_live_item(now, chat, turn, ItemKind::Tool(record), Window::open(now)),
        };
        if let Some(run) = self.runs.get_mut(&chat) {
            if let Some(id) = &agent_call {
                run.tools.insert(id.clone(), item);
            }
            run.served.insert(
                item,
                Served {
                    tool,
                    report: joined,
                    keyed: agent_call.is_some(),
                },
            );
        }
        self.calls.insert(
            call,
            Call {
                chat,
                item,
                action: action.clone(),
                stage: Stage::Previewing,
                printed: Printed::default(),
            },
        );
        match action {
            Action::Edit { .. } | Action::Write { .. } => {
                out.push(Directive::Tool(ToolDirective::Preview { call, action }));
            }
            _ => self.decide(call, out),
        }
    }

    /// The item the agent's report `id` made, in `chat`, if it's one
    /// nothing CAF serves has joined yet, and still going.
    fn reported_item(&self, chat: ChatId, id: &str) -> Option<ItemId> {
        let run = self.runs.get(&chat)?;
        let item = *run.tools.get(id)?;
        if run.served.contains_key(&item) {
            return None;
        }
        self.unended_tool(chat, item).map(|_| item)
    }

    /// The oldest item an agent's report made in `chat` of a call to
    /// `tool` given `input`, that nothing has joined yet and is still
    /// going: for a call that came without the agent's id for it. Else
    /// the oldest such report that hasn't said its input yet (none, or
    /// `{}`), as adapters report a call before making it: joined for now
    /// ([`Joined::Maybe`]), until its input says whether it's this call.
    fn unjoined_report(
        &self,
        chat: ChatId,
        tool: CafTool,
        input: &Value,
    ) -> Option<(ItemId, Joined)> {
        let run = self.runs.get(&chat)?;
        // The report's input is kept as the call's is: cut, if long.
        let input = &crate::tool::capped_input(input.clone()).0;
        let mut reports: Vec<(ItemId, &String)> = (run.tools.iter())
            .filter(|(_, item)| !run.served.contains_key(item))
            .map(|(id, &item)| (item, id))
            .collect();
        reports.sort_unstable();
        let mine = |item: ItemId| {
            (self.unended_tool(chat, item)).filter(|call| call.origin == Origin::Caf(tool))
        };
        let sure = (reports.iter())
            .find(|(item, _)| mine(*item).is_some_and(|call| call.input.as_ref() == Some(input)));
        if let Some((item, _)) = sure {
            return Some((*item, Joined::Yes));
        }
        reports.into_iter().find_map(|(item, id)| {
            let call = mine(item)?;
            let unsaid = call
                .input
                .as_ref()
                .is_none_or(|input| input.is_null() || *input == json!({}));
            unsaid.then(|| (item, Joined::Maybe(id.clone())))
        })
    }

    /// The tool call `item` of `chat`'s, if it hasn't ended.
    fn unended_tool(&self, chat: ChatId, item: ItemId) -> Option<&ToolCall<A>> {
        match &self.item(chat, item)?.kind {
            ItemKind::Tool(call) if !call.status.ended() => Some(call),
            _ => None,
        }
    }

    /// The agent's report of a call that CAF refused for what it was
    /// given: it failed, saying why.
    fn fail_report(&mut self, now: Now, chat: ChatId, item: ItemId, tool: CafTool, why: &str) {
        let Some(mut call) = self.unended_tool(chat, item).cloned() else {
            return;
        };
        call.status = ToolStatus::Failed;
        call.result.push(Piece::Text(crate::tool::Text::new(why)));
        if let Some(run) = self.runs.get_mut(&chat) {
            run.served.insert(
                item,
                Served {
                    tool,
                    report: Joined::Yes,
                    keyed: true,
                },
            );
        }
        self.set_tool(chat, item, Box::new(call));
        self.end_item(now, chat, item);
    }

    /// The agent reported a call as `id`, to CAF's tool `tool` if its
    /// adapter says so, given `input` if it said: the item of the call
    /// CAF serves it's a report of, if it's one, which it then joins. A
    /// report that's of no call CAF serves, yet, is the reported calls'
    /// to keep, as any other.
    ///
    /// A report that says nothing yet of its input (none, or `{}`) joins
    /// the oldest call of its tool waiting for one, for now: once it says
    /// its input, it stays if that's the call's, and else is let go to
    /// join the call it is, or be an item of its own.
    ///
    /// What the agent reports of a call CAF serves changes nothing of it:
    /// it added only its id, and when it started, if it came first.
    pub(super) fn served_report(
        &mut self,
        chat: ChatId,
        id: &str,
        tool: Option<CafTool>,
        input: Option<&Value>,
    ) -> Option<ItemId> {
        // An adapter may report a call before its input, as `{}`.
        let input = input.filter(|input| !matches!(input, Value::Null) && *input != &json!({}));
        let run = self.runs.get_mut(&chat)?;
        if let Some(&item) = run.tools.get(id) {
            let served = run.served.get_mut(&item)?;
            if served.report != Joined::Maybe(id.to_owned()) {
                served.report = Joined::Yes;
                return Some(item);
            }
            let Some(input) = input else {
                return Some(item);
            };
            let same = matches!(self.item(chat, item).map(|each| &each.kind),
                Some(ItemKind::Tool(call)) if call.input.as_ref() == Some(input));
            let run = self.runs.get_mut(&chat)?;
            if same {
                if let Some(served) = run.served.get_mut(&item) {
                    served.report = Joined::Yes;
                }
                return Some(item);
            }
            // It was another call's report: let go, it joins its own.
            run.tools.remove(id);
            if let Some(served) = run.served.get_mut(&item) {
                served.report = Joined::No;
            }
        }
        // A call that came without the agent's id for it: the oldest of
        // the same tool, with the same input, that no report has joined,
        // or, until the report says its input, any.
        let tool = tool?;
        let run = self.runs.get(&chat)?;
        // A report saying its input also takes a call another holds only
        // for now: that other is let go.
        let mut waiting: Vec<ItemId> = (run.served.iter())
            .filter(|(_, served)| {
                let free = match &served.report {
                    Joined::No => true,
                    Joined::Maybe(_) => input.is_some(),
                    Joined::Yes => false,
                };
                served.tool == tool && free && !served.keyed
            })
            .map(|(&item, _)| item)
            .collect();
        waiting.sort_unstable();
        let item = waiting.into_iter().find(|&item| {
            input.is_none_or(|input| {
                matches!(self.item(chat, item).map(|each| &each.kind),
                    Some(ItemKind::Tool(call)) if call.input.as_ref() == Some(input))
            })
        })?;
        let run = self.runs.get_mut(&chat)?;
        if let Some(Served {
            report: Joined::Maybe(other),
            ..
        }) = run.served.get(&item)
        {
            run.tools.remove(&other.clone());
        }
        run.tools.insert(id.to_owned(), item);
        if let Some(served) = run.served.get_mut(&item) {
            served.report = match input {
                Some(_) => Joined::Yes,
                None => Joined::Maybe(id.to_owned()),
            };
        }
        Some(item)
    }

    /// Whether `item` of `chat`'s is a call CAF serves, whose record the
    /// agent's reports never change.
    pub(super) fn is_served(&self, chat: ChatId, item: ItemId) -> bool {
        self.runs
            .get(&chat)
            .is_some_and(|run| run.served.contains_key(&item))
    }

    /// A call's preview came: its pieces go on it, and the policy is
    /// asked.
    fn previewed(&mut self, call: CallId, pieces: Vec<Piece>, out: &mut Vec<Directive<A, D>>) {
        let Some(each) = self.calls.get(&call) else {
            return stale(out, call, "a preview");
        };
        if each.stage != Stage::Previewing {
            return stale(out, call, "a second preview");
        }
        let (chat, item) = (each.chat, each.item);
        self.change_tool(chat, item, |tool| tool.result = pieces);
        self.decide(call, out);
    }

    /// Asks the policy about `call`.
    fn decide(&mut self, call: CallId, out: &mut Vec<Directive<A, D>>) {
        let Some(each) = self.calls.get(&call) else {
            return;
        };
        let (chat, item, action) = (each.chat, each.item, each.action.clone());
        let Some(ItemKind::Tool(tool)) = self.item(chat, item).map(|each| &each.kind) else {
            return;
        };
        let request = PermissionRequest {
            item: Some(item),
            title: tool.title.clone(),
            origin: tool.origin.clone(),
            action: Some(action),
            input: tool.input.as_ref().map(|input| match input {
                // Already cut to its start.
                Value::String(text) if tool.input_cut.is_some() => text.clone(),
                input => input.to_string(),
            }),
        };
        let ticket = Ticket(self.ids.take());
        if let Some(each) = self.calls.get_mut(&call) {
            each.stage = Stage::Deciding(ticket);
        }
        out.push(Directive::Policy(PolicyDirective::Decide {
            ticket,
            chat,
            request,
        }));
    }

    /// The policy answered about a call CAF serves, if `ticket` is one's.
    /// Returns whether it was.
    pub(super) fn served_policy(
        &mut self,
        now: Now,
        ticket: Ticket,
        answer: &PolicyAnswer,
        reason: Option<&str>,
        out: &mut Vec<Directive<A, D>>,
    ) -> bool {
        let Some((&call, each)) =
            (self.calls.iter()).find(|(_, each)| each.stage == Stage::Deciding(ticket))
        else {
            return false;
        };
        let by = DecidedBy::Policy {
            reason: reason.map(|reason| agent_text(reason, MOST_TITLE)),
        };
        match answer {
            PolicyAnswer::Allow => self.run_call(call, by, out),
            PolicyAnswer::AskUser if self.host.shows.permission_questions => {
                let (chat, item) = (each.chat, each.item);
                let title = match self.item(chat, item).map(|each| &each.kind) {
                    Some(ItemKind::Tool(tool)) => tool.title.clone(),
                    _ => String::new(),
                };
                let option = |(id, label): (&str, &str), kind| PermissionOption {
                    id: OptionId(id.to_owned()),
                    label: label.to_owned(),
                    kind,
                };
                let question = self.ask(
                    now,
                    chat,
                    QuestionKind::Permission(Permission {
                        tool: Some(item),
                        title,
                        options: vec![
                            option(ALLOW, OptionKind::AllowOnce),
                            option(REFUSE, OptionKind::RejectOnce),
                        ],
                    }),
                );
                if let Some(each) = self.calls.get_mut(&call) {
                    each.stage = Stage::Asking(question);
                }
                self.change_tool(chat, item, |tool| tool.status = ToolStatus::Asking);
            }
            // Refused, or no one can be asked.
            _ => self.refuse_call(now, call, by, out),
        }
        true
    }

    /// The user answered `question`, if it's about a call CAF serves,
    /// with `option`. `None` if it isn't one's; else whether the option
    /// is one it offered.
    pub(super) fn served_answer(
        &mut self,
        now: Now,
        question: QuestionId,
        option: &OptionId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Option<bool> {
        let (&call, _) =
            (self.calls.iter()).find(|(_, each)| each.stage == Stage::Asking(question))?;
        let by = |(_, label): (&str, &str)| DecidedBy::User {
            option: label.to_owned(),
        };
        match option.0.as_str() {
            id if id == ALLOW.0 => self.run_call(call, by(ALLOW), out),
            id if id == REFUSE.0 => self.refuse_call(now, call, by(REFUSE), out),
            _ => return Some(false),
        }
        Some(true)
    }

    /// Runs `call`, allowed by `by`.
    fn run_call(&mut self, call: CallId, by: DecidedBy, out: &mut Vec<Directive<A, D>>) {
        let Some(each) = self.calls.get_mut(&call) else {
            return;
        };
        let before = each.stage;
        each.stage = Stage::Running;
        let (chat, item, action) = (each.chat, each.item, each.action.clone());
        if let Stage::Asking(question) = before {
            self.unask(chat, question);
        }
        let runs_command = matches!(action, Action::Run { .. });
        self.change_tool(chat, item, |tool| {
            tool.status = ToolStatus::Running;
            tool.decision = Decision::Allowed(by);
            if runs_command {
                tool.result.push(Piece::Output(Output {
                    lines: Vec::new(),
                    dropped: 0,
                    ending: None,
                }));
            }
        });
        out.push(Directive::Tool(ToolDirective::Run { call, action }));
    }

    /// Refuses `call`, as `by` decided.
    fn refuse_call(
        &mut self,
        now: Now,
        call: CallId,
        by: DecidedBy,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(each) = self.calls.remove(&call) else {
            return;
        };
        if let Stage::Asking(question) = each.stage {
            self.unask(each.chat, question);
        }
        self.change_tool(each.chat, each.item, |tool| {
            tool.status = ToolStatus::Refused;
            tool.decision = Decision::Refused(by);
        });
        self.end_item(now, each.chat, each.item);
        out.push(Directive::Tool(ToolDirective::Refuse {
            call,
            why: RefusedCall::NotAllowed,
        }));
    }

    /// A running command's output changed: its lines from the driver's
    /// line `from` on are `lines`.
    fn printed(
        &mut self,
        call: CallId,
        from: u64,
        lines: Vec<String>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(each) = self.calls.get(&call) else {
            return stale(out, call, "output");
        };
        if each.stage != Stage::Running {
            return stale(out, call, "output before it ran");
        }
        let (chat, item) = (each.chat, each.item);
        let Some(mut printed) = self
            .calls
            .get_mut(&call)
            .map(|each| std::mem::take(&mut each.printed))
        else {
            return;
        };
        let change = match self.output(chat, item) {
            Some(output) => printed.print(&output.lines, from, lines),
            None => {
                stale(out, call, "output of a call that prints none");
                None
            }
        };
        if let Some(each) = self.calls.get_mut(&call) {
            each.printed = printed;
        }
        let Some(change) = change else {
            return;
        };
        if change.skipped > 0 {
            report(
                out,
                Level::Warn,
                format!(
                    "call {}: the tools driver skipped {} lines of output",
                    call.0, change.skipped
                ),
            );
        }
        self.record(
            chat,
            crate::change::Change::OutputChanged {
                item,
                from: change.from,
                lines: change.lines,
                dropped: change.dropped,
            },
        );
    }

    /// The output of `chat`'s call `item`, if it has one.
    fn output(&self, chat: ChatId, item: ItemId) -> Option<&Output> {
        let ItemKind::Tool(tool) = &self.item(chat, item)?.kind else {
            return None;
        };
        tool.result.iter().rev().find_map(|piece| match piece {
            Piece::Output(output) => Some(output),
            _ => None,
        })
    }

    /// A call finished, and the agent was answered.
    fn finished(
        &mut self,
        now: Now,
        call: CallId,
        done: bool,
        pieces: Vec<Piece>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        match self.calls.get(&call) {
            None => return stale(out, call, "its finish"),
            Some(each) if each.stage != Stage::Running => {
                return stale(out, call, "a finish before it ran");
            }
            Some(_) => {}
        }
        let Some(each) = self.calls.remove(&call) else {
            return;
        };
        self.change_tool(each.chat, each.item, |tool| {
            tool.status = if done {
                ToolStatus::Done
            } else {
                ToolStatus::Failed
            };
            // A diff it gives is the change as made, in place of the
            // preview's.
            if pieces.iter().any(|piece| matches!(piece, Piece::Diff(_))) {
                tool.result.retain(|kept| !matches!(kept, Piece::Diff(_)));
            }
            for piece in pieces {
                // A command's output is the one kept as it ran, ended.
                if let Piece::Output(ended) = &piece
                    && let Some(output) = (tool.result.iter_mut().rev()).find_map(|kept| match kept
                    {
                        Piece::Output(output) => Some(output),
                        _ => None,
                    })
                {
                    output.ending = ended.ending;
                    continue;
                }
                tool.result.push(piece);
            }
        });
        self.end_item(now, each.chat, each.item);
    }

    /// The agent cancelled a call, or its connection closed.
    fn gone(&mut self, now: Now, call: CallId, out: &mut Vec<Directive<A, D>>) {
        let Some(each) = self.calls.remove(&call) else {
            return stale(out, call, "its going");
        };
        if let Some(ticket) =
            self.drop_call(now, &each, ToolStatus::Cancelled, Some(StoppedBy::Agent))
        {
            out.push(Directive::Policy(PolicyDirective::Withdrawn { ticket }));
        }
    }

    /// Ends `each` as `status`, stopped `by` someone if it was cancelled: its question is taken away, its output,
    /// if it has one, ends not saying how, and its item ends. Returns the
    /// ticket of the policy's decision on it to withdraw, if it was still
    /// deciding.
    fn drop_call(
        &mut self,
        now: Now,
        each: &Call,
        status: ToolStatus,
        by: Option<StoppedBy>,
    ) -> Option<Ticket> {
        if let Stage::Asking(question) = each.stage {
            self.unask(each.chat, question);
        }
        self.change_tool(each.chat, each.item, |tool| {
            tool.status = status;
            tool.stopped_by = by.filter(|_| status == ToolStatus::Cancelled);
            for piece in &mut tool.result {
                if let Piece::Output(output) = piece
                    && output.ending.is_none()
                {
                    output.ending = Some(Ending::Unknown);
                }
            }
        });
        self.end_item(now, each.chat, each.item);
        match each.stage {
            Stage::Deciding(ticket) => Some(ticket),
            _ => None,
        }
    }

    /// `chat`'s turn `turn` is ending, `stopped` by the user or not: its
    /// calls CAF serves that haven't finished are cancelled, and end
    /// Cancelled if it was stopped, and Failed otherwise. The drivers are
    /// told when this call into the core finishes.
    pub(super) fn settle_calls(&mut self, now: Now, chat: ChatId, turn: TurnId, stopped: bool) {
        let in_turn = |item: ItemId| {
            self.turn(chat, turn)
                .is_some_and(|each| each.items.iter().any(|kept| kept.id == item))
        };
        let calls: Vec<CallId> = (self.calls.iter())
            .filter(|(_, each)| each.chat == chat && in_turn(each.item))
            .map(|(&call, _)| call)
            .collect();
        let status = if stopped {
            ToolStatus::Cancelled
        } else {
            ToolStatus::Failed
        };
        for call in calls {
            if let Some(each) = self.calls.remove(&call) {
                self.owed.push(Owed::Cancel(call));
                if let Some(ticket) = self.drop_call(now, &each, status, Some(StoppedBy::User)) {
                    self.owed.push(Owed::Withdrawn(ticket));
                }
            }
        }
    }

    /// `chat`'s turn is being stopped: its calls CAF serves that haven't
    /// started running are cancelled at once, so nothing allows them
    /// after. Those running are stopped as the turn ends.
    pub(super) fn stop_calls(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let calls: Vec<CallId> = (self.calls.iter())
            .filter(|(_, each)| each.chat == chat && each.stage != Stage::Running)
            .map(|(&call, _)| call)
            .collect();
        for call in calls {
            if let Some(each) = self.calls.remove(&call) {
                out.push(Directive::Tool(ToolDirective::Cancel { call }));
                if let Some(ticket) =
                    self.drop_call(now, &each, ToolStatus::Cancelled, Some(StoppedBy::User))
                {
                    out.push(Directive::Policy(PolicyDirective::Withdrawn { ticket }));
                }
            }
        }
    }

    /// Forgets `chat`'s calls CAF serves, as it's let go: any that
    /// haven't finished are cancelled. Its turns have ended, so there
    /// should be none.
    pub(super) fn forget_calls(&mut self, chat: ChatId) {
        let calls: Vec<CallId> = (self.calls.iter())
            .filter(|(_, each)| each.chat == chat)
            .map(|(&call, _)| call)
            .collect();
        for call in calls {
            if let Some(each) = self.calls.remove(&call) {
                if let Stage::Deciding(ticket) = each.stage {
                    self.owed.push(Owed::Withdrawn(ticket));
                }
                self.owed.push(Owed::Cancel(call));
            }
        }
    }
}

/// Reports a driver's report of `call` ignored, as the core has finished
/// with it, or it came out of order.
fn stale<A: AppItem, D: AppData>(out: &mut Vec<Directive<A, D>>, call: CallId, what: &str) {
    report(
        out,
        Level::Debug,
        format!(
            "ignored {what} of call {}, which isn't waiting for it",
            call.0
        ),
    );
}

/// A change to a command's kept output, in the record's terms.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Printing {
    /// How many of the driver's lines it skipped, which it never gave.
    pub(super) skipped: u64,
    /// The first kept line replaced.
    pub(super) from: u64,
    /// The lines from there.
    pub(super) lines: Vec<String>,
    /// How many then go from the top.
    pub(super) dropped: u64,
}

impl Printed {
    /// The driver's lines from `from` on are now `lines`, and `kept` are
    /// the lines kept: what changes in them, if anything. Lines longer
    /// than [`MOST_LINE`] are split, and lines dropped from the top to
    /// keep them within [`MOST_OUTPUT`].
    ///
    /// Lines the driver skips, starting past its last, are never kept:
    /// the lines before them stay as they are, and the driver's numbers
    /// count on from there. A line rewritten that was already dropped, or
    /// is before such a gap, rewrites from the first kept; the lines
    /// counted dropped then stay counted, so the count is at most a few
    /// lines too high.
    pub(super) fn print(
        &mut self,
        kept: &[String],
        from: u64,
        lines: Vec<String>,
    ) -> Option<Printing> {
        let printed = self.first + self.pieces.len() as u64;
        if from == printed && lines.is_empty() {
            return None;
        }
        let mut skipped = 0;
        if from > printed {
            skipped = from - printed;
            self.fixed += self.pieces.iter().sum::<u64>();
            self.pieces.clear();
            self.first = from;
        }
        let (at, kept_from) = if from < self.first {
            // Rewriting lines dropped, or fixed: all that's kept goes.
            self.first = from;
            self.fixed = 0;
            (0, 0)
        } else {
            let at = usize::try_from(from - self.first).unwrap_or(usize::MAX);
            let kept_from: u64 = self.fixed + self.pieces.iter().take(at).sum::<u64>();
            (at, kept_from)
        };
        self.pieces.truncate(at);
        let mut new = Vec::new();
        for line in lines {
            let before = new.len();
            split_line(line, &mut new);
            self.pieces.push_back((new.len() - before) as u64);
        }
        let kept_from_at = usize::try_from(kept_from)
            .unwrap_or(usize::MAX)
            .min(kept.len());
        let mut size = output_size(kept[..kept_from_at].iter().chain(&new));
        let mut dropped = 0u64;
        let mut all = kept[..kept_from_at].iter().chain(&new);
        while size > MOST_OUTPUT {
            let Some(line) = all.next() else { break };
            size -= output_size([line]);
            dropped += 1;
        }
        // Drop those lines: the fixed first, then the driver's lines'
        // pieces.
        let fixed = self.fixed.min(dropped);
        self.fixed -= fixed;
        let mut left = dropped - fixed;
        while left > 0 {
            let Some(first) = self.pieces.front_mut() else {
                break;
            };
            if *first <= left {
                left -= *first;
                self.pieces.pop_front();
                self.first += 1;
            } else {
                *first -= left;
                left = 0;
            }
        }
        Some(Printing {
            skipped,
            from: kept_from,
            lines: new,
            dropped,
        })
    }
}

/// Splits `line` into lines of at most [`MOST_LINE`] bytes, at character
/// boundaries, onto `into`.
fn split_line(mut line: String, into: &mut Vec<String>) {
    while line.len() > MOST_LINE {
        let at = line.floor_char_boundary(MOST_LINE);
        let rest = line.split_off(at);
        into.push(line);
        line = rest;
    }
    into.push(line);
}

/// What a call to `tool` given `input` does, read as Claude Code's own
/// tools take theirs (the model is trained on them), its paths worked
/// out against `cwd`: or why it doesn't make an action, for the model.
pub(super) fn action_of(tool: CafTool, input: &Value, cwd: &str) -> Result<Action, String> {
    let Some(fields) = input.as_object() else {
        return Err("The input must be an object.".to_owned());
    };
    let text = |name: &str| -> Result<Option<&str>, String> {
        match fields.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(format!("`{name}` must be a string.")),
        }
    };
    let needed = |name: &str| -> Result<&str, String> {
        text(name)?.ok_or_else(|| format!("`{name}` is missing."))
    };
    let number = |name: &str| -> Result<Option<u64>, String> {
        match fields.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .or_else(|| {
                    value
                        .as_f64()
                        .filter(|n| n.fract() == 0.0 && *n >= 0.0)
                        .map(|n| n as u64)
                })
                .map(Some)
                .ok_or_else(|| format!("`{name}` must be a whole number, 0 or more.")),
        }
    };
    let flag = |name: &str| -> Result<bool, String> {
        match fields.get(name) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(format!("`{name}` must be true or false.")),
        }
    };
    let path = || -> Result<String, String> {
        let path = needed("file_path")?;
        if path.is_empty() {
            return Err("`file_path` is empty.".to_owned());
        }
        if path.contains('\0') {
            return Err("`file_path` has a NUL in it.".to_owned());
        }
        Ok(resolve(cwd, path))
    };
    Ok(match tool {
        CafTool::Read => {
            let path = path()?;
            let offset = number("offset")?;
            let limit = number("limit")?;
            if limit == Some(0) {
                return Err("`limit` must be 1 or more.".to_owned());
            }
            let lines = (offset.is_some() || limit.is_some()).then(|| {
                let first = offset.unwrap_or(1).max(1);
                Lines {
                    first,
                    last: limit.map(|limit| first.saturating_add(limit - 1)),
                }
            });
            Action::Read { path, lines }
        }
        CafTool::Edit => {
            let path = path()?;
            needed("old_string")?;
            needed("new_string")?;
            flag("replace_all")?;
            Action::Edit { path }
        }
        CafTool::Write => {
            let path = path()?;
            needed("content")?;
            Action::Write { path }
        }
        CafTool::Bash => {
            let command = needed("command")?;
            if command.trim().is_empty() {
                return Err("`command` is empty.".to_owned());
            }
            number("timeout")?;
            text("description")?;
            Action::Run {
                command: command.to_owned(),
                folder: cwd.to_owned(),
                background: flag("run_in_background")?,
            }
        }
    })
}

/// `path` as an absolute path, worked out against `cwd` if it's relative,
/// with every `.` and `..` worked out, by its text alone: nothing is
/// looked at on disk.
pub(super) fn resolve(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let whole = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{cwd}/{path}")
    };
    for part in whole.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The title CAF gives a call doing `action` in a chat in `cwd`: its
/// file, relative to the chat's folder if it's in it, or its command's
/// first line.
pub(super) fn title_of(action: &Action, cwd: &str) -> String {
    let shown = |path: &str| {
        let folder = cwd.trim_end_matches('/');
        match path
            .strip_prefix(folder)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            Some(rest) if !rest.is_empty() => rest.to_owned(),
            _ => path.to_owned(),
        }
    };
    let title = match action {
        Action::Read { path, .. } => format!("Read {}", shown(path)),
        Action::Edit { path } => format!("Edit {}", shown(path)),
        Action::Write { path } => format!("Write {}", shown(path)),
        Action::Run { command, .. } => {
            let mut lines = command.trim().lines();
            let first = lines.next().unwrap_or_default().to_owned();
            if lines.next().is_some() {
                format!("{first} …")
            } else {
                first
            }
        }
        Action::Tool { name } => name.clone(),
    };
    agent_text(&title, MOST_TITLE)
}

/// The files a call doing `action` is about.
fn locations_of(action: &Action) -> Vec<crate::tool::Location> {
    let at = |path: &String, line| crate::tool::Location {
        path: path.clone(),
        line,
    };
    match action {
        Action::Read { path, lines } => vec![at(path, lines.map(|lines| lines.first))],
        Action::Edit { path } | Action::Write { path } => vec![at(path, None)],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn paths_are_worked_out_against_the_folder_by_their_text() {
        assert_eq!(resolve("/work", "src/main.rs"), "/work/src/main.rs");
        assert_eq!(resolve("/work", "./src/../lib.rs"), "/work/lib.rs");
        assert_eq!(resolve("/work", "/etc/./x/../passwd"), "/etc/passwd");
        assert_eq!(resolve("/work", "../../../.."), "/");
        assert_eq!(resolve("/work/", "a//b"), "/work/a/b");
    }

    #[test]
    fn claude_codes_inputs_read_as_actions() {
        let read = |input| action_of(CafTool::Read, &input, "/work");
        assert_eq!(
            read(json!({ "file_path": "src/a.rs", "offset": 10, "limit": 5 })),
            Ok(Action::Read {
                path: "/work/src/a.rs".to_owned(),
                lines: Some(Lines {
                    first: 10,
                    last: Some(14)
                }),
            })
        );
        assert_eq!(
            read(json!({ "file_path": "/x" })),
            Ok(Action::Read {
                path: "/x".to_owned(),
                lines: None
            })
        );
        assert!(read(json!({ "file_path": 3 })).is_err());
        assert!(read(json!({})).is_err());
        assert!(read(json!({ "file_path": "/x", "limit": 0 })).is_err());
        assert!(read(json!("/x")).is_err());
        assert!(
            action_of(
                CafTool::Edit,
                &json!({ "file_path": "/x", "old_string": "a" }),
                "/w"
            )
            .is_err()
        );
        assert_eq!(
            action_of(
                CafTool::Bash,
                &json!({ "command": "cargo test", "run_in_background": true, "timeout": 1000 }),
                "/work"
            ),
            Ok(Action::Run {
                command: "cargo test".to_owned(),
                folder: "/work".to_owned(),
                background: true,
            })
        );
        assert!(action_of(CafTool::Bash, &json!({ "command": "  " }), "/w").is_err());
    }

    #[test]
    fn the_listed_schemas_require_exactly_what_the_parser_does() {
        let list = crate::mcp::tool_list(&CafTool::ALL);
        for (tool, definition) in CafTool::ALL
            .into_iter()
            .zip(list.as_array().expect("a list"))
        {
            let schema = &definition["inputSchema"];
            let properties = schema["properties"].as_object().expect("properties");
            let required: Vec<&str> = (schema["required"].as_array().expect("required").iter())
                .map(|name| name.as_str().expect("a name"))
                .collect();
            // A value of each field's type that the parser takes.
            let value = |name: &str| match properties[name]["type"].as_str() {
                Some("string") => json!("x"),
                Some("number") => json!(5),
                Some("boolean") => json!(true),
                other => panic!("{tool:?}.{name} is of type {other:?}"),
            };
            let only = |names: &[&str]| -> Value {
                names
                    .iter()
                    .map(|&name| (name.to_owned(), value(name)))
                    .collect()
            };
            assert!(
                action_of(tool, &only(&required), "/w").is_ok(),
                "{tool:?} takes its required fields alone"
            );
            for missing in &required {
                let fewer: Vec<&str> = required
                    .iter()
                    .copied()
                    .filter(|name| name != missing)
                    .collect();
                assert!(
                    action_of(tool, &only(&fewer), "/w").is_err(),
                    "{tool:?} needs {missing}"
                );
            }
            let all: Vec<&str> = properties.keys().map(String::as_str).collect();
            assert!(
                action_of(tool, &only(&all), "/w").is_ok(),
                "{tool:?} takes every field of its schema, of its type"
            );
            for name in &all {
                let mut wrong = only(&all);
                wrong[*name] = json!({});
                assert!(
                    action_of(tool, &wrong, "/w").is_err(),
                    "{tool:?}.{name} is typed"
                );
            }
        }
    }

    #[test]
    fn titles_say_what_a_call_does_from_the_chats_folder() {
        let path = |path: &str| Action::Edit {
            path: path.to_owned(),
        };
        assert_eq!(
            title_of(&path("/work/src/main.rs"), "/work"),
            "Edit src/main.rs"
        );
        assert_eq!(title_of(&path("/workshop/a"), "/work"), "Edit /workshop/a");
        assert_eq!(title_of(&path("/work"), "/work"), "Edit /work");
        let run = Action::Run {
            command: "cargo test\ncargo build".to_owned(),
            folder: "/work".to_owned(),
            background: false,
        };
        assert_eq!(title_of(&run, "/work"), "cargo test …");
    }

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|&line| line.to_owned()).collect()
    }

    /// Applies `printing` to `kept`, as a viewer would.
    fn apply(kept: &mut Vec<String>, printing: &Printing) {
        kept.truncate(usize::try_from(printing.from).expect("small"));
        kept.extend(printing.lines.iter().cloned());
        kept.drain(..usize::try_from(printing.dropped).expect("small"));
    }

    #[test]
    fn output_grows_and_a_progress_bar_rewrites_its_line() {
        let mut printed = Printed::default();
        let mut kept = Vec::new();
        let change = printed
            .print(&kept, 0, lines(&["one", "10%"]))
            .expect("a change");
        apply(&mut kept, &change);
        let change = printed.print(&kept, 1, lines(&["50%"])).expect("a change");
        assert_eq!(
            change,
            Printing {
                skipped: 0,
                from: 1,
                lines: lines(&["50%"]),
                dropped: 0
            }
        );
        apply(&mut kept, &change);
        assert_eq!(kept, lines(&["one", "50%"]));
        assert_eq!(printed.print(&kept, 2, Vec::new()), None);
    }

    #[test]
    fn output_keeps_its_end_and_splits_overlong_lines() {
        let mut printed = Printed::default();
        let mut kept = Vec::new();
        let long = "é".repeat(MOST_LINE); // 2 bytes each
        let change = printed
            .print(&kept, 0, vec![long.clone()])
            .expect("a change");
        assert_eq!(change.lines.len(), 2);
        assert!(change.lines.iter().all(|line| line.len() <= MOST_LINE));
        assert_eq!(change.lines.concat(), long);
        apply(&mut kept, &change);
        // Enough more to drop past the long line's first piece, each line
        // costing its ending too.
        let block = "x".repeat(MOST_LINE - 2);
        let more: Vec<String> = (0..3).map(|_| block.clone()).collect();
        let change = printed.print(&kept, 1, more).expect("a change");
        assert_eq!(change.from, 2);
        assert_eq!(change.dropped, 1);
        apply(&mut kept, &change);
        assert!(output_size(&kept) <= MOST_OUTPUT);
        assert_eq!(printed.first, 0, "the long line is partly kept");
        assert_eq!(printed.pieces, [1, 1, 1, 1]);
        // Rewriting the line partly dropped rewrites what's kept of it.
        let change = printed
            .print(&kept, 0, lines(&["short"]))
            .expect("a change");
        assert_eq!(
            change,
            Printing {
                skipped: 0,
                from: 0,
                lines: lines(&["short"]),
                dropped: 0
            }
        );
        apply(&mut kept, &change);
        assert_eq!(kept, lines(&["short"]));
        // Then all of it goes, and a rewrite before the first kept line
        // rewrites from the top.
        let many: Vec<String> = (0..5).map(|_| block.clone()).collect();
        let change = printed.print(&kept, 1, many).expect("a change");
        apply(&mut kept, &change);
        assert_eq!(change.dropped, 2);
        assert_eq!(printed.first, 2);
        let change = printed
            .print(&kept, 0, lines(&["again"]))
            .expect("a change");
        assert_eq!(
            change,
            Printing {
                skipped: 0,
                from: 0,
                lines: lines(&["again"]),
                dropped: 0
            }
        );
        assert_eq!(printed.first, 0);
    }
}

#[cfg(test)]
mod squashing {
    use super::*;
    use crate::change::Change;
    use crate::chat::NoAppItems;
    use crate::core::saving::squash_output;

    /// Applies `change`, an output change, to `kept`, its lines and how
    /// many were dropped, as the record does.
    fn apply((kept, gone): &mut (Vec<String>, u64), change: &Change<NoAppItems>) {
        let Change::OutputChanged {
            from,
            lines,
            dropped,
            ..
        } = change
        else {
            panic!("an output change");
        };
        let from = usize::try_from(*from).expect("small");
        let dropped = usize::try_from(*dropped).expect("small");
        assert!(from <= kept.len() && dropped <= from + lines.len());
        kept.truncate(from);
        kept.extend(lines.iter().cloned());
        kept.drain(..dropped);
        *gone += dropped as u64;
    }

    #[test]
    fn a_run_of_output_changes_squashes_to_one_doing_the_same() {
        let block = |n: usize| "x".repeat(MOST_LINE / 2 + n);
        // Every pair of a few outputs: growing, rewriting, dropping.
        let steps: Vec<(u64, Vec<String>)> = vec![
            (0, vec!["a".into(), "b".into(), "c".into()]),
            (2, vec!["C".into()]),
            (3, (0..6).map(block).collect()),
            (1, vec!["B".into(), "c".into()]),
            (5, (0..5).map(block).collect()),
            (0, vec!["top".into()]),
            (13, vec!["end".into()]),
        ];
        for start in 0..steps.len() {
            let mut printed = Printed::default();
            let mut kept = (Vec::new(), 0);
            // Up to `start` as it was, then the rest squashed into one.
            let mut squashed: Option<Change<NoAppItems>> = None;
            let mut before = (Vec::new(), 0);
            for (at, (from, lines)) in steps.iter().enumerate() {
                let Some(change) = printed.print(&kept.0, *from, lines.clone()) else {
                    continue;
                };
                let change = Change::OutputChanged {
                    item: ItemId(1),
                    from: change.from,
                    lines: change.lines,
                    dropped: change.dropped,
                };
                if at == start {
                    before = kept.clone();
                }
                apply(&mut kept, &change);
                if at < start {
                    continue;
                }
                match &mut squashed {
                    Some(last) => assert!(squash_output(last, &change)),
                    None => squashed = Some(change),
                }
            }
            if let Some(squashed) = squashed {
                apply(&mut before, &squashed);
                assert_eq!(before, kept, "squashed from step {start}");
            }
        }
        // Another call's changes aren't squashed.
        let mut one = Change::<NoAppItems>::OutputChanged {
            item: ItemId(1),
            from: 0,
            lines: Vec::new(),
            dropped: 0,
        };
        let other = Change::OutputChanged {
            item: ItemId(2),
            from: 0,
            lines: Vec::new(),
            dropped: 0,
        };
        assert!(!squash_output(&mut one, &other));
    }
}

#[cfg(test)]
mod empty_lines {
    use super::*;

    #[test]
    fn empty_lines_count_their_endings_so_the_output_stays_bounded() {
        let mut printed = Printed::default();
        let change = printed
            .print(&[], 0, vec![String::new(); 1_000_000])
            .expect("a change");
        let kept = 1_000_000 - change.dropped;
        assert_eq!(kept, MOST_OUTPUT as u64, "each empty line costs its ending");
    }
}

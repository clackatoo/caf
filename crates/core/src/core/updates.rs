//! `session/update`: what the agent says as it works, turned into items.
//!
//! Every update is routed by its session id, whether or not a prompt is
//! running: output outside any prompt goes into a turn the agent started
//! by itself. An update CAF can't read, or reads but doesn't use yet
//! within a turn, becomes an unknown item holding the original, so
//! nothing is lost. ACP's list of updates has no catch-all, so each is
//! read by its tag, and read leniently.
//!
//! While a `session/load` replays a session's history, updates build
//! imported turns instead, and nothing in them acts.

use agent_client_protocol_schema::v1 as acp;
use serde_json::Value;

use super::clean::{agent_text, unhidden};
use super::state::Open;
use super::{Core, report};
use crate::chat::AppData;
use crate::chat::{
    AppItem, Cost, Item, ItemKind, Part, Starter, ToolCall, ToolKind, ToolStatus, Turn, TurnLife,
    Unknown, Usage, UserMessage,
};
use crate::directive::{Directive, Level};
use crate::ids::{ChatId, ItemId, Ticket, TurnId};
use crate::profiles::ProfileChoice;
use crate::time::{End, Now, Window};
use crate::tool::{
    Cut, Decision, Diff, Link, Location, MOST_DIFF_WORK, Origin, Output, Piece, ServedTool, Text,
    capped_input, capped_locations, image, reported_action,
};

/// The longest tool title kept, in bytes.
const MOST_TITLE: usize = 1024;

/// The biggest whole number JavaScript reads exactly, 2^53 - 1: every
/// number in a chat is at most this.
pub(super) const MOST_EXACT: u64 = (1 << 53) - 1;

/// What an update does to a chat.
enum Said {
    /// Words, or thinking if `thinking`.
    Text { text: String, thinking: bool },
    /// A tool call, new or changed, by the agent's id for it.
    Tool { id: String, change: ToolChange },
    /// How full the agent's memory is, and the model the agent says it's
    /// using, if it says.
    Usage { usage: Usage, model: Option<String> },
    /// The account's limits alone, which are heard from the update
    /// itself: nothing of the agent's memory.
    Limits,
    /// The user's own message, as the agent tells it.
    User(String),
    /// What the agent offers of its settings, as ACP's session config
    /// options: the whole list, if it could be read.
    Options(Option<Vec<Value>>),
    /// Something about the whole chat (plan, commands, mode, title), kept
    /// by a later feature.
    Later(String),
    /// Something CAF can't read, or doesn't use yet.
    Unknown(Unknown),
}

/// A tool call as an update gives it: everything for a new one, or only
/// what changed.
struct ToolChange {
    title: Option<String>,
    kind: Option<ToolKind>,
    status: Option<ToolStatus>,
    origin: Option<Origin>,
    /// Its raw input, capped, and how much was kept if it was cut.
    input: Option<(Value, Option<Cut>)>,
    result: Option<Vec<Piece>>,
    /// The locations kept, and how many more there were.
    locations: Option<(Vec<Location>, u64)>,
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Takes in a `session/update` from `proc`.
    pub(super) fn update(
        &mut self,
        now: Now,
        proc: Ticket,
        params: &Value,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let session = params.get("sessionId").and_then(Value::as_str);
        let Some(chat) = session.and_then(|session| self.chat_with_session(proc, session)) else {
            report(
                out,
                Level::Debug,
                "ignored an update for a session no chat holds".to_owned(),
            );
            return;
        };
        let update = params.get("update").cloned().unwrap_or(Value::Null);
        let profile = self.profile_of_chat(chat);
        let replaying = self.runs.get(&chat).is_some_and(|run| run.replaying);
        // The account's limits, heard from what it says now, never from
        // history: read from the update itself, so they're heard even if
        // the rest of it isn't.
        if let ProfileChoice::Builtin(builtin) = profile
            && !replaying
            && update.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update")
        {
            self.limits_heard(now, builtin, &update, out);
        }
        let said = self.read_update(chat, &profile, update);
        if replaying {
            self.replayed(now, chat, said, out);
        } else {
            self.live(now, chat, said, out);
        }
    }

    /// The chat whose session `session` is on `proc`, open or opening.
    pub(super) fn chat_with_session(&self, proc: Ticket, session: &str) -> Option<ChatId> {
        (self.runs.iter())
            .find(|(_, run)| {
                let on = matches!(run.open, Open::Live { proc: on } | Open::Asked { proc: on, .. } if on == proc);
                on && run.session.as_ref().is_some_and(|held| held.0 == session)
            })
            .map(|(&chat, _)| chat)
    }

    /// What `update` says, read by its tag.
    fn read_update(&self, chat: ChatId, profile: &ProfileChoice, update: Value) -> Said {
        let tag = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let read = match tag.as_str() {
            "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk" => {
                text_of(&update).map(|text| match tag.as_str() {
                    "user_message_chunk" => Said::User(text),
                    thought => Said::Text {
                        text,
                        thinking: thought == "agent_thought_chunk",
                    },
                })
            }
            "tool_call" | "tool_call_update" => {
                let servers = (self.runs.get(&chat))
                    .map(super::sessions::server_names)
                    .unwrap_or_default();
                tool_call(profile, &servers, &update, tag == "tool_call")
            }
            // A number JavaScript can't read exactly makes it unreadable:
            // every number in a chat is below 2^53.
            "usage_update" if profile.limits_only(&update) => Some(Said::Limits),
            "usage_update" => serde_json::from_value::<acp::UsageUpdate>(update.clone())
                .ok()
                .filter(|usage| usage.used <= MOST_EXACT && usage.size <= MOST_EXACT)
                .map(|usage| Said::Usage {
                    usage: Usage {
                        used: usage.used,
                        size: usage.size,
                        cost: usage.cost.map(|cost| Cost {
                            amount: cost.amount,
                            currency: agent_text(&cost.currency, 16),
                        }),
                    },
                    model: profile.model_in_usage(&update),
                }),
            "config_option_update" => Some(Said::Options(
                update
                    .get("configOptions")
                    .and_then(Value::as_array)
                    .cloned(),
            )),
            "plan"
            | "available_commands_update"
            | "current_mode_update"
            | "session_info_update" => Some(Said::Later(tag.clone())),
            // Notices, compaction, and anything newer than CAF.
            _ => None,
        };
        read.unwrap_or_else(|| {
            let what = if tag.is_empty() {
                "session/update".to_owned()
            } else {
                agent_text(&tag, 64)
            };
            Said::Unknown(Unknown {
                what,
                raw: update,
                summary: None,
                cut: None,
            })
        })
    }

    /// Takes in what the agent says live.
    fn live(&mut self, now: Now, chat: ChatId, said: Said, out: &mut Vec<Directive<A, D>>) {
        // Anything the agent says keeps a turn it started going.
        let quiet = self.host.timers.quiet;
        if let Some(run) = self.runs.get_mut(&chat)
            && run.agent_turn.is_some()
        {
            run.quiet_by = Some(now.mono.after(quiet));
        }
        match said {
            Said::Text { thinking: true, .. } if !self.host.shows.thinking => {}
            Said::Text { text, .. } if text.trim().is_empty() && self.open_turn(chat).is_none() => {
                report(
                    out,
                    Level::Debug,
                    "ignored blank words outside any turn".to_owned(),
                );
            }
            Said::Text { text, thinking } => {
                let Some(turn) = self.live_turn(now, chat, out) else {
                    return;
                };
                let last = self.turn(chat, turn).and_then(|each| each.items.last());
                let grows = last
                    .filter(|item| item.window.end == End::Open)
                    .and_then(|item| match &item.kind {
                        ItemKind::Words(_) if !thinking => Some(item.id),
                        ItemKind::Thinking(_) if thinking => Some(item.id),
                        _ => None,
                    });
                match grows {
                    Some(item) => self.append(chat, item, &text),
                    None => {
                        let kind = if thinking {
                            ItemKind::Thinking(text)
                        } else {
                            ItemKind::Words(text)
                        };
                        self.add_live_item(now, chat, turn, kind, Window::open(now));
                    }
                }
            }
            Said::Tool { id, change } => self.live_tool(now, chat, id, change, out),
            Said::Usage { usage, model } => {
                self.heard_usage(chat, &usage, model);
                self.set_usage(chat, usage);
            }
            Said::Limits => {}
            Said::User(_) => report(
                out,
                Level::Debug,
                "the agent told the user's message back: not kept twice".to_owned(),
            ),
            Said::Options(options) => self.options_updated(now, chat, options.as_ref(), out),
            Said::Later(what) => report(
                out,
                Level::Debug,
                format!("not kept yet, until a later feature: {what}"),
            ),
            // Not the agent's own output: it never opens a turn.
            Said::Unknown(unknown) => match self.open_turn(chat) {
                Some(turn) => {
                    self.add_live_item(
                        now,
                        chat,
                        turn,
                        ItemKind::Unknown(unknown),
                        Window::instant(now),
                    );
                }
                None => report(
                    out,
                    Level::Info,
                    format!(
                        "chat {}: the agent said, outside any turn: {}",
                        chat.0,
                        said_of(&unknown)
                    ),
                ),
            },
        }
    }

    /// The tool call a permission request names, `call`: the one CAF has
    /// seen, or a new one from what the request says of it.
    pub(super) fn tool_named(
        &mut self,
        now: Now,
        chat: ChatId,
        call: &Value,
        out: &mut Vec<Directive<A, D>>,
    ) -> Option<ItemId> {
        let id = call.get("toolCallId").and_then(Value::as_str)?;
        if let Some(item) = self.runs.get(&chat).and_then(|run| run.tools.get(id)) {
            return Some(*item);
        }
        let mut update = call.clone();
        update["sessionUpdate"] = Value::from("tool_call_update");
        let Said::Tool { id, change } = self.read_update(chat, &self.profile_of_chat(chat), update)
        else {
            return None;
        };
        self.live_tool(now, chat, id.clone(), change, out);
        self.runs
            .get(&chat)
            .and_then(|run| run.tools.get(&id))
            .copied()
    }

    /// Adds an item to a live turn, ending words or thinking before it.
    pub(super) fn add_live_item(
        &mut self,
        now: Now,
        chat: ChatId,
        turn: TurnId,
        kind: ItemKind<A>,
        window: Window,
    ) -> ItemId {
        let last = self.turn(chat, turn).and_then(|each| each.items.last());
        let streaming = last.filter(|item| {
            item.window.end == End::Open
                && matches!(item.kind, ItemKind::Words(_) | ItemKind::Thinking(_))
        });
        if let Some(item) = streaming.map(|item| item.id) {
            self.end_item(now, chat, item);
        }
        self.add_item(chat, turn, kind, window)
    }

    /// A tool call, new or changed. A change to one CAF has seen lands on
    /// its own item, in whichever turn that is.
    fn live_tool(
        &mut self,
        now: Now,
        chat: ChatId,
        id: String,
        change: ToolChange,
        out: &mut Vec<Directive<A, D>>,
    ) {
        // A tool of CAF's server that isn't CAF's own is one of the app's
        // own, if the chat was given it: the adapter can't know which.
        let mut change = change;
        if let Some(Origin::Server { server, tool }) = &change.origin
            && server == crate::profiles::CAF_SERVER
            && (self.runs.get(&chat)).is_some_and(|run| run.app_tools.contains(tool))
        {
            change.origin = Some(Origin::App(tool.clone()));
        }
        // A report of a call CAF serves changes nothing of it.
        let served = match &change.origin {
            Some(Origin::Caf(tool)) => Some(ServedTool::Caf(*tool)),
            Some(Origin::App(name)) => Some(ServedTool::App(name.clone())),
            _ => None,
        };
        let input = change.input.as_ref().map(|(input, _)| input);
        if self.served_report(chat, &id, served, input).is_some() {
            return;
        }
        // While the user stops a turn, Claude's adapter says each call it
        // didn't finish failed, as ACP's first version has no other word
        // for it, before it answers the prompt: they were cancelled.
        let stopping = (self.runs.get(&chat))
            .and_then(|run| run.running.as_ref())
            .is_some_and(|running| running.stop_by.is_some());
        if stopping && change.status == Some(ToolStatus::Failed) {
            change.status = Some(ToolStatus::Cancelled);
        }
        let known = self
            .runs
            .get(&chat)
            .and_then(|run| run.tools.get(&id).copied());
        let current = known.and_then(|item| match &self.item(chat, item)?.kind {
            ItemKind::Tool(tool) => Some(tool.clone()),
            _ => None,
        });
        match (known, current) {
            (Some(item), Some(mut tool)) => {
                change.apply(&mut tool);
                let done = tool.status.ended();
                self.set_tool(chat, item, tool);
                if done {
                    self.end_item(now, chat, item);
                }
            }
            _ if self
                .runs
                .get(&chat)
                .is_some_and(|run| run.dropped_tools.contains(&id)) =>
            {
                report(
                    out,
                    Level::Debug,
                    format!(
                        "chat {}: dropped an update to a tool call dropped as in flight",
                        chat.0
                    ),
                );
            }
            _ => {
                let Some(turn) = self.live_turn(now, chat, out) else {
                    if let Some(run) = self.runs.get_mut(&chat) {
                        run.dropped_tools.insert(id);
                    }
                    return;
                };
                let tool = change.new_tool();
                let done = tool.status.ended();
                let window = if done {
                    Window::instant(now)
                } else {
                    Window::open(now)
                };
                let item =
                    self.add_live_item(now, chat, turn, ItemKind::Tool(Box::new(tool)), window);
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.tools.insert(id, item);
                }
            }
        }
    }

    /// Takes in what the agent replays of a session's history: turns are
    /// built and added once each is whole. Nothing in them acts, and their
    /// times aren't known.
    fn replayed(&mut self, now: Now, chat: ChatId, said: Said, out: &mut Vec<Directive<A, D>>) {
        // Each update replayed shows the agent is still at it.
        if let Some(super::state::Open::Asked { proc, id }) =
            self.runs.get(&chat).map(|run| &run.open)
            && let Some(pending) = self.pending.get_mut(&(*proc, *id))
            && pending.deadline.is_some()
        {
            pending.deadline = Some(now.mono.after(self.host.timers.session_open));
        }
        // A restored chat has its record: what's replayed is only for the
        // agent to continue from.
        if self.runs.get(&chat).is_some_and(|run| run.restored) {
            return;
        }
        let kind = match said {
            Said::User(text) => return self.replayed_user(chat, text),
            Said::Text { thinking: true, .. } if !self.host.shows.thinking => return,
            Said::Text { text, thinking } => {
                if self.grow_import(chat, &text, thinking) {
                    return;
                }
                if thinking {
                    ItemKind::Thinking(text)
                } else {
                    ItemKind::Words(text)
                }
            }
            Said::Tool { id, change } => {
                let known = self
                    .runs
                    .get(&chat)
                    .and_then(|run| run.tools.get(&id).copied());
                let run = self.runs.get_mut(&chat);
                let found = run
                    .and_then(|run| run.import.as_mut())
                    .and_then(|turn| turn.items.iter_mut().find(|item| Some(item.id) == known));
                if let Some(Item {
                    kind: ItemKind::Tool(tool),
                    ..
                }) = found
                {
                    change.apply(tool);
                    return;
                }
                let item = self.import_item(chat, ItemKind::Tool(Box::new(change.new_tool())));
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.tools.insert(id, item);
                }
                return;
            }
            Said::Usage { usage, .. } => return self.set_usage(chat, usage),
            Said::Limits => return,
            // What the session offers is in the answer that opens it.
            Said::Options(_) => {
                return report(
                    out,
                    Level::Debug,
                    "ignored settings said while the session was replayed".to_owned(),
                );
            }
            Said::Later(what) => {
                return report(
                    out,
                    Level::Debug,
                    format!("not kept yet, until a later feature: {what}"),
                );
            }
            Said::Unknown(unknown) => ItemKind::Unknown(unknown),
        };
        self.import_item(chat, kind);
    }

    /// A replayed message from the user: it starts a turn, unless the turn
    /// it started has nothing yet, when it's another part of it.
    fn replayed_user(&mut self, chat: ChatId, text: String) {
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        if let Some(Turn {
            starter: Starter::User(message),
            items,
            ..
        }) = &mut run.import
            && items.is_empty()
        {
            message.parts.push(Part::Text(text));
            return;
        }
        self.flush_import(chat);
        self.start_import(chat, Starter::User(UserMessage::text(text)));
    }

    /// Adds to the last replayed words or thinking, if that's what the
    /// imported turn ends with. Returns whether it did.
    fn grow_import(&mut self, chat: ChatId, text: &str, thinking: bool) -> bool {
        let last = (self.runs.get_mut(&chat))
            .and_then(|run| run.import.as_mut())
            .and_then(|turn| turn.items.last_mut());
        match last.map(|item| &mut item.kind) {
            Some(ItemKind::Words(words)) if !thinking => words.push_str(text),
            Some(ItemKind::Thinking(words)) if thinking => words.push_str(text),
            _ => return false,
        }
        true
    }

    /// Starts an imported turn.
    fn start_import(&mut self, chat: ChatId, starter: Starter) {
        let id = TurnId(self.ids.take());
        if let Some(run) = self.runs.get_mut(&chat) {
            run.import = Some(Turn {
                id,
                starter,
                life: TurnLife::Finished,
                window: Window::unknown(),
                items: Vec::new(),
                imported: true,
                model: None,
                spent: None,
            });
        }
    }

    /// Adds an item to the imported turn, starting one the agent began if
    /// there's none.
    fn import_item(&mut self, chat: ChatId, kind: ItemKind<A>) -> ItemId {
        if self.runs.get(&chat).is_some_and(|run| run.import.is_none()) {
            self.start_import(chat, Starter::Agent);
        }
        let id = ItemId(self.ids.take());
        if let Some(turn) = self.runs.get_mut(&chat).and_then(|run| run.import.as_mut()) {
            turn.items.push(Item {
                id,
                window: Window::unknown(),
                parent: None,
                kind,
            });
        }
        id
    }
}

impl ToolChange {
    /// Changes `tool` as this says. A call that has ended stays as it
    /// ended, whatever the agent says of it after, though what it gave back
    /// may still come; one is Asking while its permission question is open,
    /// unless the agent says it has gone on. Whose tool it is is set once,
    /// and what it does with it, as far as CAF knows that. An update's
    /// result replaces the last: an agent sends what it has so far, such as
    /// Claude's diff of the edit's input, then the file's own.
    fn apply<A: AppItem>(self, tool: &mut ToolCall<A>) {
        if let Some(title) = self.title {
            tool.title = agent_text(&title, MOST_TITLE);
        }
        if let Some(kind) = self.kind {
            tool.kind = kind;
        }
        if let Some(origin) = self.origin
            && tool.origin == Origin::NotRecorded
        {
            tool.origin = origin;
        }
        if tool.action.is_none() {
            tool.action = reported_action(&tool.origin);
        }
        if let Some(status) = self.status {
            let stands = tool.status.ended()
                || (tool.status == ToolStatus::Asking && status == ToolStatus::Pending);
            if !stands {
                tool.status = status;
            }
        }
        if let Some((input, cut)) = self.input {
            tool.input = Some(input);
            tool.input_cut = cut;
        }
        if let Some(result) = self.result {
            tool.result = result;
        }
        if let Some((locations, more)) = self.locations {
            tool.locations = locations;
            tool.locations_cut = more;
        }
        tool.settle_output();
    }

    /// A new tool call, as this says.
    fn new_tool<A: AppItem>(self) -> ToolCall<A> {
        let mut tool = ToolCall {
            title: String::new(),
            kind: ToolKind::Other,
            status: ToolStatus::Pending,
            origin: Origin::NotRecorded,
            action: None,
            input: None,
            input_cut: None,
            result: Vec::new(),
            locations: Vec::new(),
            locations_cut: 0,
            decision: Decision::NotAsked,
            app: None,
            stopped_by: None,
            job: None,
        };
        self.apply(&mut tool);
        tool
    }
}

/// What an update CAF couldn't place says, for a report: its kind, and
/// for a notice its title and description, which are written for the
/// user. Cleaned and capped, as the agent's words.
fn said_of(unknown: &Unknown) -> String {
    let text = |key: &str| {
        unknown
            .raw
            .get(key)
            .and_then(Value::as_str)
            .map(|text| agent_text(text, MOST_NOTICE))
    };
    match (text("title"), text("description")) {
        (Some(title), Some(description)) => format!("{}: {title}: {description}", unknown.what),
        (Some(title), None) => format!("{}: {title}", unknown.what),
        _ => unknown.what.clone(),
    }
}

/// The longest title or description of a notice reported, in bytes.
const MOST_NOTICE: usize = 1024;

/// A tool call, all of it if `whole`, or a change to one: read as far as
/// it goes, so a call missing something ACP requires, such as its title,
/// is still a call its updates find. Its status, kind, content and
/// locations are read apart, and leniently, so one CAF can't read loses
/// nothing else: a piece of content kept as it came, a location with only
/// its path, or counted as cut. Its origin is what the adapter says of it,
/// as its profile reads that against the chat's MCP servers, `servers`,
/// and never its title.
fn tool_call(
    profile: &ProfileChoice,
    servers: &[String],
    update: &Value,
    whole: bool,
) -> Option<Said> {
    let mut fields = update.clone();
    let object = fields.as_object_mut()?;
    let content = object
        .remove("content")
        .filter(|content| !content.is_null());
    let locations = object
        .remove("locations")
        .filter(|locations| !locations.is_null());
    let status = object.remove("status");
    let kind = object.remove("kind");
    let call = serde_json::from_value::<acp::ToolCallUpdate>(fields).ok()?;
    let fields = call.fields;
    let places = locations.as_ref().map(read_locations);
    let result = match (content.as_ref(), fields.raw_output.as_ref()) {
        (Some(content), raw) => Some(pieces(content, raw, places.as_ref())),
        (None, Some(raw)) => Some(raw_output(raw).into_iter().collect()),
        (None, None) => whole.then(Vec::new),
    };
    let status = match status.as_ref().and_then(Value::as_str) {
        Some("pending") => Some(ToolStatus::Pending),
        Some("in_progress") => Some(ToolStatus::Running),
        Some("completed") => Some(ToolStatus::Done),
        Some("failed") => Some(ToolStatus::Failed),
        // ACP's next version says a call was cancelled.
        Some("cancelled") => Some(ToolStatus::Cancelled),
        // Newer than CAF: its status stays as it was.
        _ => None,
    };
    let kind = kind
        .and_then(|kind| serde_json::from_value::<acp::ToolKind>(kind).ok())
        .map(tool_kind);
    Some(Said::Tool {
        id: call.tool_call_id.0.to_string(),
        change: ToolChange {
            title: fields.title,
            kind: kind.or(whole.then_some(ToolKind::Other)),
            status: status.or(whole.then_some(ToolStatus::Pending)),
            origin: profile.origin_in(update, servers),
            input: fields.raw_input.map(capped_input),
            result,
            locations: places.map(|(places, unread)| {
                let (kept, more) = capped_locations(places.into_iter().flatten().collect());
                (kept, more + unread)
            }),
        },
    })
}

/// The longest path kept, in bytes.
const MOST_PATH: usize = 4096;

/// The locations a call is about, as ACP gives them, each in its place:
/// `None` for one with no path, and how many of those there were. One
/// whose line can't be read keeps its path; a list that isn't one is one
/// that can't be read.
fn read_locations(locations: &Value) -> (Vec<Option<Location>>, u64) {
    let Some(locations) = locations.as_array() else {
        return (Vec::new(), 1);
    };
    let read: Vec<Option<Location>> = (locations.iter())
        .map(|location| {
            let path = location.get("path")?.as_str()?;
            Some(Location {
                path: agent_text(path, MOST_PATH),
                line: (location.get("line").and_then(Value::as_u64))
                    .filter(|line| (1..=MOST_EXACT).contains(line)),
            })
        })
        .collect();
    let unread = read.iter().filter(|place| place.is_none()).count() as u64;
    (read, unread)
}

/// What a tool gave back, `content`, in order: a piece for each item of
/// it, one CAF can't read kept as it came. Its raw output, `raw`, if it
/// has no content. A call has one output at most: a second terminal is
/// kept as it came. A diff is numbered from the line of the location in
/// the same place in `places`, if that's of the same file: an agent may
/// send only the part of a file that changed, as Claude's adapter does,
/// with where it is.
fn pieces(
    content: &Value,
    raw: Option<&Value>,
    places: Option<&(Vec<Option<Location>>, u64)>,
) -> Vec<Piece> {
    let Some(content) = content.as_array() else {
        return vec![unknown_piece(content)];
    };
    if content.is_empty() {
        return raw.and_then(raw_output).into_iter().collect();
    }
    let mut output = false;
    // What working out their diffs may cost, between them all.
    let mut budget = MOST_DIFF_WORK;
    (content.iter().enumerate())
        .map(|(at, each)| {
            match serde_json::from_value::<acp::ToolCallContent>(each.clone()) {
                Ok(acp::ToolCallContent::Content(content)) => match content.content {
                    acp::ContentBlock::Text(text) => Piece::Text(Text::new(unhidden(&text.text))),
                    acp::ContentBlock::Image(picture) => Piece::Image(image(
                        agent_text(&picture.mime_type, MOST_MIME),
                        picture.data,
                    )),
                    acp::ContentBlock::ResourceLink(link) => Piece::Link(Link {
                        uri: agent_text(&link.uri, MOST_PATH),
                        name: agent_text(&link.name, MOST_TITLE),
                    }),
                    _ => unknown_piece(each),
                },
                Ok(acp::ToolCallContent::Diff(diff)) => {
                    let path = agent_text(&diff.path.to_string_lossy(), MOST_PATH);
                    let line = (places.and_then(|(places, _)| places.get(at)))
                        .and_then(Option::as_ref)
                        .filter(|place| place.path == path)
                        .and_then(|place| place.line);
                    let mut diff =
                        Diff::within(path, diff.old_text.as_deref(), &diff.new_text, &mut budget);
                    if let Some(line) = line {
                        diff.number_from(line);
                    }
                    Piece::Diff(diff)
                }
                // A terminal CAF never gave the agent: its output isn't
                // CAF's to show, but there was one.
                Ok(acp::ToolCallContent::Terminal(_)) if !output => {
                    output = true;
                    Piece::Output(Output {
                        lines: Vec::new(),
                        dropped: 0,
                        ending: None,
                    })
                }
                Ok(_) | Err(_) => unknown_piece(each),
            }
        })
        .collect()
}

/// The longest image type kept, in bytes.
const MOST_MIME: usize = 128;

/// A piece of content CAF can't read, kept as it came: the start of its
/// JSON, as a string, if that's longer than
/// [`MOST_INPUT`](crate::tool::MOST_INPUT).
fn unknown_piece(content: &Value) -> Piece {
    let what = content
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("content");
    let (raw, cut) = capped_input(content.clone());
    Piece::Unknown(Unknown {
        what: agent_text(what, 64),
        raw,
        summary: None,
        cut,
    })
}

/// A tool's raw output as text, if it gave any.
fn raw_output(raw: &Value) -> Option<Piece> {
    let text = match raw {
        Value::Null => return None,
        Value::String(text) => unhidden(text),
        raw => unhidden(&raw.to_string()),
    };
    (!text.is_empty()).then(|| Piece::Text(Text::new(text)))
}

/// Imported tool calls that hadn't ended didn't finish, as far as anyone
/// knows.
pub(super) fn settle_imported<A: AppItem>(turn: &mut Turn<A>) {
    for item in &mut turn.items {
        if let ItemKind::Tool(tool) = &mut item.kind
            && !tool.status.ended()
        {
            tool.status = ToolStatus::Failed;
            tool.settle_output();
        }
    }
}

/// The text of a chunk update: `None` if it isn't text.
fn text_of(update: &Value) -> Option<String> {
    match serde_json::from_value::<acp::ContentChunk>(update.clone())
        .ok()?
        .content
    {
        acp::ContentBlock::Text(text) => Some(text.text),
        _ => None,
    }
}

/// ACP's kind of tool in CAF's terms.
fn tool_kind(kind: acp::ToolKind) -> ToolKind {
    match kind {
        acp::ToolKind::Read => ToolKind::Read,
        acp::ToolKind::Edit => ToolKind::Edit,
        acp::ToolKind::Delete => ToolKind::Delete,
        acp::ToolKind::Move => ToolKind::Move,
        acp::ToolKind::Search => ToolKind::Search,
        acp::ToolKind::Execute => ToolKind::Execute,
        acp::ToolKind::Think => ToolKind::Think,
        acp::ToolKind::Fetch => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

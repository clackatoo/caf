//! Usage: what each turn spent, recorded as it ends, and what each chat
//! has spent, priced with the table the usage driver last gave.
//!
//! A turn's record is made from the agent's answer to its prompt, as the
//! profile reads it, what the agent said of its usage as the turn ran,
//! and the session's cost as the agent last said it before the turn was
//! sent and as it ended. A session opened, new or again, counts its cost
//! from nothing. Nothing estimated is kept on
//! a turn: a chat's spend is worked out from its turns, again whenever the
//! prices change and whenever a chat is restored.

use serde_json::Value;

use super::clean::agent_text;
use super::updates::MOST_EXACT;
use super::{Core, report};
use crate::change::Change;
use crate::chat::{AppData, AppItem, Chat, Cost, Usage};
use crate::directive::{Directive, Level, UsageDirective};
use crate::ids::{ChatId, TurnId};
use crate::profiles::{Reckoned, Reported};
use crate::settings::{SettingKind, SettingValue, Settings};
use crate::time::Now;
use crate::usage::{LedgerLine, ModelTokens, PriceTable, Spend, TurnUsage};

/// The longest model id kept, in bytes.
const MOST_MODEL: usize = 256;

/// The model a row is put against when neither the agent's split nor the
/// turn says which it was.
const UNKNOWN_MODEL: &str = "unknown";

/// What was so of a turn when it was sent, that its record needs.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub(super) struct Sent {
    /// The session's cost, as the agent last said it.
    pub(super) cost: Option<Cost>,
    /// Whether fast mode was on, by the settings in effect.
    pub(super) fast: bool,
}

/// What the agent said of its usage while a turn ran.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub(super) struct Heard {
    /// It said what its session has cost.
    pub(super) cost: bool,
    /// The model it last said it was using.
    pub(super) model: Option<String>,
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// The agent said, live, how full its memory is, `usage`, and perhaps
    /// which model it's using, `model`: noted for the turn running, if
    /// one is.
    pub(super) fn heard_usage(&mut self, chat: ChatId, usage: &Usage, model: Option<String>) {
        let Some(running) = self
            .runs
            .get_mut(&chat)
            .and_then(|run| run.running.as_mut())
        else {
            return;
        };
        running.heard.cost |= usage.cost.is_some();
        if model.is_some() {
            running.heard.model = model;
        }
    }

    /// What's so of `chat` as a turn of it is sent.
    pub(super) fn as_sent(&self, chat: ChatId) -> Sent {
        Sent {
            cost: self.runs.get(&chat).and_then(|run| run.cost.clone()),
            fast: self
                .chats
                .get(&chat)
                .is_some_and(|whole| fast_on(&whole.settings)),
        }
    }

    /// `chat`'s turn `turn` ended at `now` with the agent's answer to its
    /// prompt, `result`, having been sent as `sent`, the agent having said
    /// what `heard` holds as it ran: what it spent is recorded, if the
    /// agent said, on the turn, in the chat's spend, and as a line of the
    /// usage ledger.
    pub(super) fn turn_spent(
        &mut self,
        now: Now,
        chat: ChatId,
        turn: TurnId,
        result: &Value,
        (sent, heard): (Sent, Heard),
        out: &mut Vec<Directive<A, D>>,
    ) {
        let rows = match self.profile_of_chat(chat).spent(result) {
            Reported::Nothing => return,
            Reported::Unreadable => {
                return report(
                    out,
                    Level::Warn,
                    format!(
                        "chat {}: what turn {} spent can't be read: it isn't recorded",
                        chat.0, turn.0
                    ),
                );
            }
            Reported::Rows(rows) => rows,
        };
        // Every count the agent gave is below 2^53, and so is what the
        // turn's are added up to.
        let within = |count: &u64| *count <= MOST_EXACT;
        let counts = |row: &Reckoned| {
            let tokens = &row.tokens;
            [
                tokens.input,
                tokens.cache_read,
                tokens.cache_write,
                tokens.output,
            ]
        };
        let readable = (rows.iter())
            .all(|row| within(&row.total) && row.tokens.thinking.is_none_or(|n| within(&n)));
        let total = (rows.iter().flat_map(counts))
            .try_fold(0_u64, u64::checked_add)
            .filter(within);
        if !readable || total.is_none() {
            return report(
                out,
                Level::Warn,
                format!(
                    "chat {}: the agent counted more than 2^53 tokens for turn {}: it isn't recorded",
                    chat.0, turn.0
                ),
            );
        }
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        let Some(each) = whole.turns.iter().find(|each| each.id == turn) else {
            return;
        };
        // A row the agent didn't say the model of is the one it said it
        // used during the turn, else the one the turn was sent on.
        let turn_model = (heard.model.map(|model| agent_text(&model, MOST_MODEL)))
            .filter(|model| !model.is_empty())
            .or_else(|| each.model.as_ref().map(|model| model.id.clone()));
        let models = (rows.into_iter())
            .map(|row| {
                let model = (row.model.map(|model| agent_text(&model, MOST_MODEL)))
                    .filter(|model| !model.is_empty())
                    .or_else(|| turn_model.clone())
                    .unwrap_or_else(|| UNKNOWN_MODEL.to_owned());
                ModelTokens {
                    model,
                    tokens: row.tokens,
                }
            })
            .collect();
        let after = self.runs.get(&chat).and_then(|run| run.cost.as_ref());
        let usage = TurnUsage {
            models,
            // Only if the agent said its cost during the turn.
            cost: heard
                .cost
                .then(|| cost_over(sent.cost.as_ref(), after))
                .flatten(),
            fast: sent.fast,
        };
        let line = LedgerLine {
            at: now.wall,
            app: self.host.name.clone(),
            chat,
            turn,
            agent: whole.agent.clone(),
            profile: self.slugs.get(&whole.agent).map(|slug| (*slug).to_owned()),
            usage: usage.clone(),
        };
        self.record(
            chat,
            Change::TurnSpent {
                turn,
                usage: Box::new(usage),
            },
        );
        self.respend(chat);
        out.push(Directive::Usage(UsageDirective::Record(Box::new(line))));
    }

    /// Works out `chat`'s spend again, with the prices the core has now,
    /// and records it if it changed.
    pub(super) fn respend(&mut self, chat: ChatId) {
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        let spend = spend_of(whole, &self.prices);
        if *whole.spend != spend {
            self.record(chat, Change::Spend(Box::new(spend)));
        }
    }

    /// The usage driver gave new prices, `prices`: every chat's spend is
    /// worked out again with them, and sent to those whose spend changed.
    pub(super) fn prices_given(&mut self, prices: PriceTable) {
        self.prices = prices;
        let chats: Vec<ChatId> = self.chats.keys().copied().collect();
        for chat in chats {
            self.respend(chat);
        }
    }
}

/// What `chat` has spent, over its turns that have a record, priced with
/// `prices`. A cost reported in US dollars counts as it was; any other
/// turn's is estimated.
pub(super) fn spend_of<A: AppItem>(chat: &Chat<A>, prices: &PriceTable) -> Spend {
    let mut spend = Spend::default();
    for usage in (chat.turns.iter()).filter_map(|turn| turn.spent.as_deref()) {
        spend.tokens.add(&usage.tokens());
        spend.turns += 1;
        match &usage.cost {
            Some(cost) if cost.currency == USD => spend.reported += cost.amount,
            _ => {
                let estimate = prices.estimate(usage);
                spend.estimated += estimate.usd;
                spend.unpriced += estimate.unpriced;
            }
        }
    }
    spend
}

/// US dollars, as ISO 4217 names them: the only currency CAF adds up.
const USD: &str = "USD";

/// What a turn cost, by the session's cost as the agent said it before the
/// turn was sent, `before`, and as it ended, `after`: how much it rose. If
/// it went down, or changed currency, the session was replaced, and the
/// turn cost what it says at the end. `None` if the agent never said, or
/// if it didn't change: Claude's adapter leaves a stopped turn out of its
/// total, though its tokens were spent, so it's estimated instead. Asked
/// only of a turn the agent said its cost during.
fn cost_over(before: Option<&Cost>, after: Option<&Cost>) -> Option<Cost> {
    let after = after?;
    if before == Some(after) {
        return None;
    }
    let amount = match before {
        Some(before) if before.currency == after.currency && before.amount <= after.amount => {
            after.amount - before.amount
        }
        _ => after.amount,
    };
    Some(Cost {
        amount,
        currency: after.currency.clone(),
    })
}

/// Whether fast mode is on, by the settings in effect: what the agent says
/// its fast mode is.
fn fast_on(settings: &Settings) -> bool {
    (settings.offered.iter()).any(|setting| {
        setting.kind == SettingKind::Fast && setting.current == Some(SettingValue::Switch(true))
    })
}

#[cfg(test)]
mod tests {
    use super::cost_over;
    use crate::chat::Cost;

    fn usd(amount: f64) -> Cost {
        Cost {
            amount,
            currency: "USD".into(),
        }
    }

    #[test]
    fn a_turns_cost_is_how_much_the_sessions_rose() {
        assert_eq!(
            cost_over(Some(&usd(0.5)), Some(&usd(0.75))),
            Some(usd(0.25))
        );
        assert_eq!(cost_over(None, Some(&usd(0.75))), Some(usd(0.75)));
        // Said, but unchanged: not reported.
        assert_eq!(cost_over(Some(&usd(0.5)), Some(&usd(0.5))), None);
        // Said for the first time, of a session opened afresh: reported.
        assert_eq!(cost_over(None, Some(&usd(0.0))), Some(usd(0.0)));
        // The session was replaced: what it says at the end.
        assert_eq!(
            cost_over(Some(&usd(0.5)), Some(&usd(0.25))),
            Some(usd(0.25))
        );
        let eur = Cost {
            amount: 0.75,
            currency: "EUR".into(),
        };
        assert_eq!(cost_over(Some(&usd(0.5)), Some(&eur)), Some(eur));
        assert_eq!(cost_over(Some(&usd(0.5)), None), None);
        assert_eq!(cost_over(None, None), None);
    }
}

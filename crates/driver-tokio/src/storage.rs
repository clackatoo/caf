//! The storage driver: the app's [`Store`], on a thread of its own.
//!
//! Each directive is done with [`caf_save::carry_out`], in the order it's
//! given, so the store's side of the contract
//! ([`StorageDirective`]) is kept: one thread does a chat's work in order,
//! and every directive with a ticket is answered once.
//!
//! The store is the app's code, so a panic in it is caught: the work it
//! was doing fails, as if the store had said so, and the core never waits
//! on an answer that won't come. The store is used again after, as the
//! core tries a failed write again until it's written, and shutting down
//! waits for that: a store that's no longer used would hold shutting down
//! up for good. A store's write is whole or not there, so one cut off by
//! a panic is as if it never happened.

use std::panic::AssertUnwindSafe;

use caf_core::directive::StorageDirective;
use caf_core::event::StorageEvent;
use caf_core::{AppData, AppItem, Ticket};
use caf_save::Store;
use tokio::sync::mpsc;

/// An answer from the store: the ticket of the directive, and how it went.
type Answer<A, D> = (Ticket, StorageEvent<A, D>);

/// Runs a store on a thread of its own, or answers for there being none.
pub(crate) struct StorageDriver<A: AppItem, D: AppData> {
    /// Work for the thread, if there's a store. Dropped to end it.
    work: Option<std::sync::mpsc::Sender<StorageDirective<A, D>>>,
    /// Where answers go: the thread has a copy.
    tell: mpsc::UnboundedSender<Answer<A, D>>,
    answers: mpsc::UnboundedReceiver<Answer<A, D>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl<A: AppItem, D: AppData> StorageDriver<A, D> {
    /// A driver for `store`, or for none.
    pub(crate) fn new(store: Option<Box<dyn Store>>) -> Self {
        let (tell, answers) = mpsc::unbounded_channel();
        let (work, thread) = match store {
            Some(store) => {
                let (work, inbox) = std::sync::mpsc::channel();
                let tell = tell.clone();
                let thread = std::thread::Builder::new()
                    .name("caf-store".into())
                    .spawn(move || keep(store, &inbox, &tell))
                    .expect("a thread for the store");
                (Some(work), Some(thread))
            }
            None => (None, None),
        };
        Self {
            work,
            tell,
            answers,
            thread,
        }
    }

    /// Whether there's a store.
    pub(crate) fn has_store(&self) -> bool {
        self.work.is_some()
    }

    /// Hands the store a directive. With no store, or once its thread has
    /// gone, it fails.
    pub(crate) fn apply(&mut self, directive: StorageDirective<A, D>) {
        let directive = match &self.work {
            Some(work) => match work.send(directive) {
                Ok(()) => return,
                Err(unsent) => unsent.0,
            },
            None => directive,
        };
        if let Some(ticket) = ticket_of(&directive) {
            let failed = StorageEvent::Failed("CAF has no store to keep chats in".into());
            let _ = self.tell.send((ticket, failed));
        }
    }

    /// The store's next answer. Cancel-safe.
    pub(crate) async fn next(&mut self) -> Answer<A, D> {
        match self.answers.recv().await {
            Some(answer) => answer,
            // Never: the driver keeps a sender.
            None => std::future::pending().await,
        }
    }

    /// Ends the thread once it has done the work it was given, and waits
    /// for it.
    pub(crate) async fn finish(mut self) {
        self.work = None;
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

/// Does each directive, in order, until there are no more.
fn keep<A: AppItem, D: AppData>(
    mut store: Box<dyn Store>,
    inbox: &std::sync::mpsc::Receiver<StorageDirective<A, D>>,
    tell: &mpsc::UnboundedSender<Answer<A, D>>,
) {
    while let Ok(directive) = inbox.recv() {
        let ticket = ticket_of(&directive);
        let done = std::panic::catch_unwind(AssertUnwindSafe(|| {
            caf_save::carry_out(&mut *store, directive)
        }));
        let answer = done.unwrap_or_else(|_| {
            log::warn!(target: "caf", "the store panicked: what it was doing failed");
            ticket.map(|ticket| (ticket, StorageEvent::Failed("the store panicked".into())))
        });
        if let Some(answer) = answer {
            // Once the task has gone, nobody needs it.
            let _ = tell.send(answer);
        }
    }
}

/// A directive's ticket, if it has one.
fn ticket_of<A: AppItem, D: AppData>(directive: &StorageDirective<A, D>) -> Option<Ticket> {
    match directive {
        StorageDirective::List { ticket, .. }
        | StorageDirective::Load { ticket, .. }
        | StorageDirective::Snapshot { ticket, .. }
        | StorageDirective::Append { ticket, .. }
        | StorageDirective::Delete { ticket, .. }
        | StorageDirective::MoveShelf { ticket, .. } => Some(*ticket),
        StorageDirective::Release { .. } => None,
        // None the core gives yet: `carry_out` answers nothing for one.
        _ => None,
    }
}

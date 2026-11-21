//! How big a long chat is saved: a guard against the format growing.

mod common;

use caf_core::save::Saved;
use caf_save::{Squeeze, encode_snapshot, recompress};
use common::{Notes, Pin, from};
use serde_json::json;

/// Text that never changes: the licences'.
const APACHE: &str = include_str!("../../../LICENSE-APACHE");
const MIT: &str = include_str!("../../../LICENSE-MIT");

/// The budgets, in bytes, each about a tenth over what was measured when
/// they were set, for [`long`]: 3,803,588 bytes of MessagePack, 152,518
/// compressed fast, and 128,192 squeezed. Raising one needs a reason.
const RAW: usize = 4_180_000;
const FAST: usize = 168_000;
const HARDEST: usize = 141_000;

/// A long working session: 300 turns of the agent's words and thinking,
/// made up of the licences' words, files read and read again, and
/// commands' output.
fn long() -> Saved<Pin, Notes> {
    let words: Vec<&str> = (APACHE.split_whitespace().chain(MIT.split_whitespace()))
        .filter(|word| word.chars().all(char::is_alphabetic))
        .collect();
    let lines: Vec<&str> = APACHE.lines().collect();
    // A fixed sequence of choices, so the chat is the same every time.
    let mut seed: u64 = 0x5eed;
    let mut next = |below: usize| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (seed >> 33) as usize % below
    };
    // Sentences of the licences' words, in no order: as hard to compress
    // as prose, near enough.
    let mut prose = |sentences: usize| {
        let mut text = String::new();
        for _ in 0..sentences {
            let length = 6 + next(14);
            let sentence: Vec<&str> = (0..length).map(|_| words[next(words.len())]).collect();
            text += &sentence.join(" ");
            text += ". ";
        }
        text
    };
    let mut turns = Vec::new();
    let mut id = 0;
    for n in 0..300 {
        id += 1;
        let turn = id;
        let mut items = Vec::new();
        id += 1;
        items.push(common::item(
            id,
            json!({ "type": "Thinking", "value": prose(4) }),
        ));
        id += 1;
        items.push(common::item(
            id,
            json!({ "type": "Words", "value": prose(6) }),
        ));
        // A file read: a window of the licence, numbered as tools do.
        let start = (n * 37) % lines.len();
        let read: String = (lines.iter().enumerate().cycle().skip(start).take(150))
            .map(|(at, line)| format!("{:>6}\t{line}\n", at + 1))
            .collect();
        id += 1;
        items.push(common::tool(id, &format!("src/file{}.rs", n % 25), &read));
        // A command's output, much the same each time.
        let output = format!(
            "running 48 tests\n{}test result: ok. 48 passed; 0 failed; finished in 0.{n:02}s\n",
            (0..48)
                .map(|test| format!("test tests::case_{test} ... ok\n"))
                .collect::<String>()
        );
        id += 1;
        items.push(common::tool(id, "cargo test", &output));
        id += 1;
        items.push(common::item(
            id,
            json!({ "type": "Words", "value": prose(3) }),
        ));
        turns.push(common::turn(turn, &prose(1), items));
    }
    from(common::saved_with(1, turns))
}

#[test]
fn a_long_chat_saves_small() {
    let saved = long();
    let raw = rmp_serde::to_vec_named(&saved).expect("it writes").len();
    let fast = encode_snapshot(&saved, Squeeze::Fast);
    let hardest = recompress(&fast, Squeeze::Hardest).expect("it squeezes");
    eprintln!(
        "{raw} bytes of MessagePack: {} fast, {} squeezed",
        fast.len(),
        hardest.len()
    );
    assert!(raw <= RAW, "{raw} bytes of MessagePack");
    assert!(fast.len() <= FAST, "{} bytes fast", fast.len());
    assert!(hardest.len() <= HARDEST, "{} bytes squeezed", hardest.len());
}

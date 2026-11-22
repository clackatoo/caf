//! Tests of the ledger: writing lines, compressing months, and summaries.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use caf_core::time::Wall;
use caf_core::usage::{
    BadQuery, Bucket, LedgerLine, ModelPrice, PriceLayer, PriceTable, PricedModel, Resolution,
    Summary, UsageQuery,
};
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::testing::Folder;
use crate::{Ledger, folder_from, summarise};

/// The app most tests write as.
const APP: &str = "clackatoo";

/// A time, from its RFC 3339 text.
fn wall(text: &str) -> Wall {
    let at: Timestamp = text.parse().expect("the test's time reads");
    Wall(at.as_millisecond())
}

/// A line's JSON, as tests change it before reading it: chat `chat`'s
/// turn `turn`, ending at `at`, by clackatoo's `claude`, with no profile,
/// of a thousand tokens in and out on Haiku, and no cost reported.
fn line(at: &str, chat: u64, turn: u64) -> Value {
    json!({
        "at": wall(at).0,
        "app": APP,
        "chat": chat,
        "turn": turn,
        "agent": "claude",
        "profile": null,
        "usage": {
            "models": [{
                "model": "claude-haiku-5-5",
                "tokens": {"input": 1000, "cache_read": 0, "cache_write": 0, "output": 1000},
            }],
            "cost": null,
            "fast": false,
        },
    })
}

/// A line, from its JSON.
fn read(value: Value) -> LedgerLine {
    serde_json::from_value(value).expect("the test's line reads")
}

/// Writes lines as `app`.
fn record(folder: &Path, app: &str, lines: impl IntoIterator<Item = Value>) {
    let ledger = Ledger::new(folder.to_owned(), app);
    for line in lines {
        ledger.record(&read(line)).expect("it's written");
    }
}

/// A table pricing Haiku at $1 and $5 a million, and Opus at $5 and $25
/// with fast mode at twice.
fn prices() -> PriceTable {
    let mut opus = ModelPrice::new(5.0, 25.0);
    opus.fast = Some(2.0);
    let mut table = PriceTable::default();
    table.models = vec![
        PricedModel::new(
            "anthropic/claude-haiku-5-5",
            ModelPrice::new(1.0, 5.0),
            PriceLayer::Bundled,
        ),
        PricedModel::new("claude-opus-5-5", opus, PriceLayer::Own),
    ];
    table.fetched = Some(Wall(42));
    table
}

/// A summary by `by`, in `zone`, from `since` until `until`.
fn summary(folder: &Path, since: &str, until: &str, zone: &str, by: Resolution) -> Summary {
    let mut query = UsageQuery::new(wall(since), wall(until), zone);
    query.by = by;
    summarise(folder, &query, &prices()).expect("the query's good")
}

/// November 2026's, by UTC day.
fn november(folder: &Path) -> Summary {
    summary(
        folder,
        "2026-11-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
        "UTC",
        Resolution::Day,
    )
}

/// Every turn a summary counts, added up.
fn turns(summary: &Summary) -> u64 {
    summary.buckets.iter().map(|bucket| bucket.turns).sum()
}

/// The names of the files in an app's folder, sorted, but its lock.
fn names(folder: &Path, app: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder.join(app))
        .expect("it's there")
        .map(|entry| {
            entry
                .expect("it reads")
                .file_name()
                .into_string()
                .expect("text")
        })
        .filter(|name| name != ".lock")
        .collect();
    names.sort();
    names
}

/// The file of `app`'s lines for `month`.
fn file(folder: &Path, app: &str, month: &str) -> PathBuf {
    folder.join(app).join(format!("{month}.jsonl"))
}

/// Floats near enough equal.
#[track_caller]
fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{actual} isn't {expected}"
    );
}

/// The writer the process tests run in each child: set by
/// `CAF_USAGE_WRITER` as `<folder>|<first chat>|<how>`. `november` writes
/// 300 chats' turns in November; `crossing` writes 150 chats' first turns
/// in October and second in November, one after the other, so each
/// November line compresses October as the other program adds to it.
#[test]
#[ignore = "a child of the process tests"]
fn writer() {
    let Some(task) = std::env::var_os("CAF_USAGE_WRITER") else {
        return;
    };
    let task = task.into_string().expect("text");
    let mut parts = task.split('|');
    let (Some(folder), Some(first), Some(how)) = (parts.next(), parts.next(), parts.next()) else {
        panic!("folder|chat|how");
    };
    let first: u64 = first.parse().expect("a number");
    let lines: Vec<Value> = match how {
        "november" => (first..first + 300)
            .map(|chat| line("2026-11-22T10:00:00Z", chat, 1))
            .collect(),
        "crossing" => (first..first + 150)
            .flat_map(|chat| {
                [
                    line("2026-10-31T10:00:00Z", chat, 1),
                    line("2026-11-01T10:00:00Z", chat, 2),
                ]
            })
            .collect(),
        how => panic!("no writer {how}"),
    };
    record(Path::new(folder), APP, lines);
}

/// Runs two writers, in programs of their own, at once.
fn two_programs(folder: &Path, how: &str) {
    let me = std::env::current_exe().expect("the test's program");
    let children: Vec<_> = [0, 10_000]
        .into_iter()
        .map(|first| {
            std::process::Command::new(&me)
                .args(["--exact", "tests::writer", "--ignored", "--quiet"])
                .env(
                    "CAF_USAGE_WRITER",
                    format!("{}|{first}|{how}", folder.display()),
                )
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("it starts")
        })
        .collect();
    for mut child in children {
        assert!(child.wait().expect("it ends").success());
    }
}

#[test]
fn two_programs_crossing_a_month_lose_no_line() {
    let folder = Folder::new("processes-crossing");
    two_programs(folder.path(), "crossing");
    let all = summary(
        folder.path(),
        "2026-10-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
        "UTC",
        Resolution::Day,
    );
    assert_eq!(turns(&all), 600);
    assert_eq!(all.chats.all, 300);
    assert!(all.unreadable.is_empty());
}

#[test]
fn two_programs_never_mix_lines() {
    let folder = Folder::new("processes");
    two_programs(folder.path(), "november");
    let text = std::fs::read_to_string(file(folder.path(), APP, "2026-11")).expect("it's there");
    assert_eq!(text.lines().count(), 600);
    assert!(
        text.lines()
            .all(|line| serde_json::from_str::<LedgerLine>(line).is_ok())
    );
    let summary = november(folder.path());
    assert_eq!(turns(&summary), 600);
    assert!(summary.unreadable.is_empty());
}

#[test]
fn two_writers_never_mix_lines() {
    let folder = Folder::new("threads");
    std::thread::scope(|scope| {
        for first in [0, 10_000, 20_000] {
            let path = folder.path();
            scope.spawn(move || {
                record(
                    path,
                    APP,
                    (first..first + 200).map(|chat| line("2026-11-22T10:00:00Z", chat, 1)),
                );
            });
        }
    });
    let text = std::fs::read_to_string(file(folder.path(), APP, "2026-11")).expect("it's there");
    assert_eq!(text.lines().count(), 600);
    assert!(
        text.lines()
            .all(|line| serde_json::from_str::<LedgerLine>(line).is_ok())
    );
    assert_eq!(november(folder.path()).chats.all, 600);
}

#[test]
fn a_line_cut_off_by_a_crash_is_skipped_and_written_over() {
    let folder = Folder::new("cut");
    record(
        folder.path(),
        APP,
        [
            line("2026-11-22T10:00:00Z", 1, 1),
            line("2026-11-22T11:00:00Z", 1, 2),
        ],
    );
    let path = file(folder.path(), APP, "2026-11");
    let mut bytes = std::fs::read(&path).expect("it's there");
    bytes.extend_from_slice(b"{\"at\":17637");
    std::fs::write(&path, &bytes).expect("it's written");

    let summary = november(folder.path());
    assert_eq!(turns(&summary), 2);
    assert!(summary.unreadable.is_empty());

    // The next line goes where the cut one was, not on the end of it.
    record(folder.path(), APP, [line("2026-11-22T12:00:00Z", 1, 3)]);
    let text = std::fs::read_to_string(&path).expect("it's there");
    assert_eq!(text.lines().count(), 3);
    assert!(text.ends_with('\n'));
    let summary = november(folder.path());
    assert_eq!(turns(&summary), 3);
    assert!(summary.unreadable.is_empty());
}

#[test]
fn a_file_with_an_unreadable_line_before_its_last_is_named() {
    let folder = Folder::new("unreadable-line");
    record(folder.path(), APP, [line("2026-11-22T10:00:00Z", 1, 1)]);
    let path = file(folder.path(), APP, "2026-11");
    let mut bytes = std::fs::read(&path).expect("it's there");
    bytes.extend_from_slice(b"not a line\n");
    std::fs::write(&path, &bytes).expect("it's written");
    record(folder.path(), APP, [line("2026-11-22T11:00:00Z", 1, 2)]);

    let summary = november(folder.path());
    assert_eq!(turns(&summary), 2, "the lines that read still count");
    assert_eq!(summary.unreadable, ["clackatoo/2026-11.jsonl"]);
}

#[test]
fn files_that_cant_be_read_are_named() {
    let folder = Folder::new("unreadable-files");
    record(folder.path(), APP, [line("2026-11-22T10:00:00Z", 1, 1)]);
    let eyespy = folder.path().join("eyespy");
    std::fs::create_dir_all(&eyespy).expect("it's made");
    std::fs::write(eyespy.join("2026-10.jsonl.zst"), b"not zstd").expect("it's written");
    // Out of the period: never read, so never named.
    std::fs::write(eyespy.join("2026-09.jsonl.zst"), b"not zstd").expect("it's written");

    let summary = summary(
        folder.path(),
        "2026-10-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
        "UTC",
        Resolution::Day,
    );
    assert_eq!(turns(&summary), 1);
    assert_eq!(summary.unreadable, ["eyespy/2026-10.jsonl.zst"]);
}

#[test]
fn a_month_is_compressed_when_a_later_one_is_written() {
    let folder = Folder::new("rollover");
    record(
        folder.path(),
        APP,
        (1..=50).map(|turn| line("2026-10-31T23:00:00Z", 1, turn)),
    );
    let october = |folder: &Path| {
        summary(
            folder,
            "2026-10-01T00:00:00Z",
            "2026-11-01T00:00:00Z",
            "Europe/London",
            Resolution::Hour,
        )
    };
    let before = october(folder.path());
    assert_eq!(turns(&before), 50);
    assert_eq!(names(folder.path(), APP), ["2026-10.jsonl"]);

    record(folder.path(), APP, [line("2026-11-01T00:00:01Z", 1, 51)]);
    assert_eq!(
        names(folder.path(), APP),
        ["2026-10.jsonl.zst", "2026-11.jsonl"]
    );
    assert_eq!(october(folder.path()), before);
    let packed =
        std::fs::read(folder.path().join(APP).join("2026-10.jsonl.zst")).expect("it's there");
    let lines = zstd::decode_all(packed.as_slice()).expect("it's zstd");
    assert_eq!(lines.iter().filter(|byte| **byte == b'\n').count(), 50);
}

#[test]
fn a_turn_ending_in_a_month_compressed_still_counts() {
    let folder = Folder::new("late");
    record(
        folder.path(),
        APP,
        [
            line("2026-10-31T23:59:59Z", 1, 1),
            line("2026-11-01T00:00:01Z", 2, 1),
            // Recorded after November's first: October's is compressed.
            line("2026-10-31T23:59:58Z", 3, 1),
        ],
    );
    assert_eq!(
        names(folder.path(), APP),
        ["2026-10.jsonl", "2026-10.jsonl.zst", "2026-11.jsonl"]
    );
    let october = |folder: &Path| {
        summary(
            folder,
            "2026-10-01T00:00:00Z",
            "2026-11-01T00:00:00Z",
            "UTC",
            Resolution::Day,
        )
    };
    let before = october(folder.path());
    assert_eq!(before.chats.all, 2);

    // The next month's first line joins them.
    record(folder.path(), APP, [line("2026-12-01T00:00:00Z", 4, 1)]);
    assert_eq!(
        names(folder.path(), APP),
        ["2026-10.jsonl.zst", "2026-11.jsonl.zst", "2026-12.jsonl"]
    );
    assert_eq!(october(folder.path()), before);
}

#[test]
fn a_month_left_both_ways_by_a_crash_counts_once() {
    let folder = Folder::new("both");
    record(
        folder.path(),
        APP,
        [
            line("2026-10-30T10:00:00Z", 1, 1),
            line("2026-10-30T11:00:00Z", 1, 2),
            line("2026-11-01T10:00:00Z", 1, 3),
        ],
    );
    // As a crash between the rename and the removal leaves it.
    let app = folder.path().join(APP);
    let packed = std::fs::read(app.join("2026-10.jsonl.zst")).expect("it's there");
    let lines = zstd::decode_all(packed.as_slice()).expect("it's zstd");
    std::fs::write(app.join("2026-10.jsonl"), &lines).expect("it's written");

    let october = |folder: &Path| {
        summary(
            folder,
            "2026-10-01T00:00:00Z",
            "2026-11-01T00:00:00Z",
            "UTC",
            Resolution::Day,
        )
    };
    assert_eq!(turns(&october(folder.path())), 2);

    record(folder.path(), APP, [line("2026-11-01T11:00:00Z", 1, 4)]);
    assert_eq!(
        names(folder.path(), APP),
        ["2026-10.jsonl.zst", "2026-11.jsonl"]
    );
    let packed = std::fs::read(app.join("2026-10.jsonl.zst")).expect("it's there");
    assert_eq!(
        zstd::decode_all(packed.as_slice()).expect("it's zstd"),
        lines
    );
    assert_eq!(turns(&october(folder.path())), 2);
}

#[test]
fn two_copies_crossing_a_month_compress_it_once() {
    let folder = Folder::new("crossing");
    record(
        folder.path(),
        APP,
        (1..=20).map(|turn| line("2026-10-31T12:00:00Z", 1, turn)),
    );
    std::thread::scope(|scope| {
        for chat in [2, 3] {
            let path = folder.path();
            scope.spawn(move || {
                record(
                    path,
                    APP,
                    (1..=20).map(|turn| line("2026-11-01T12:00:00Z", chat, turn)),
                );
            });
        }
    });
    assert_eq!(
        names(folder.path(), APP),
        ["2026-10.jsonl.zst", "2026-11.jsonl"]
    );
    let all = summary(
        folder.path(),
        "2026-10-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
        "UTC",
        Resolution::Day,
    );
    assert_eq!(turns(&all), 60);
    assert!(all.unreadable.is_empty());
}

#[test]
fn a_turn_counts_once_and_its_first_line_counts() {
    let folder = Folder::new("twice");
    let mut second = line("2026-11-22T10:00:00Z", 1, 1);
    second["usage"]["models"][0]["tokens"]["input"] = json!(5);
    record(
        folder.path(),
        APP,
        [line("2026-11-22T10:00:00Z", 1, 1), second],
    );
    // And from another app's folder, as if it wrote the same.
    let mut theirs = line("2026-11-22T10:00:00Z", 1, 1);
    theirs["app"] = json!("eyespy");
    record(folder.path(), "eyespy", [theirs]);

    let summary = november(folder.path());
    assert_eq!(turns(&summary), 1);
    assert_eq!(summary.buckets[0].tokens.input, 1000);
}

#[test]
fn lines_with_fields_a_later_caf_added_are_read() {
    let folder = Folder::new("later");
    let mut later = line("2026-11-22T10:00:00Z", 1, 1);
    later["region"] = json!("eu");
    later["usage"]["models"][0]["tokens"]["audio"] = json!(12);
    let app = folder.path().join(APP);
    std::fs::create_dir_all(&app).expect("it's made");
    std::fs::write(app.join("2026-11.jsonl"), format!("{later}\n")).expect("it's written");
    record(folder.path(), APP, [line("2026-11-22T11:00:00Z", 1, 2)]);

    let summary = november(folder.path());
    assert_eq!(turns(&summary), 2);
    assert!(summary.unreadable.is_empty());
    // And the line as written is never changed.
    let text = std::fs::read_to_string(app.join("2026-11.jsonl")).expect("it's there");
    assert_eq!(text.lines().next(), Some(later.to_string().as_str()));
}

#[test]
fn days_begin_in_the_querys_time_zone_whatever_the_clocks_do() {
    let folder = Folder::new("days");
    record(
        folder.path(),
        APP,
        [
            // 29 March 2026: the clocks go forward at 01:00 GMT.
            line("2026-03-29T00:30:00Z", 1, 1),
            line("2026-03-29T22:30:00Z", 1, 2),
            // 30 March: 00:30 BST.
            line("2026-03-29T23:30:00Z", 1, 3),
        ],
    );
    let spring = summary(
        folder.path(),
        "2026-03-01T00:00:00Z",
        "2026-04-01T00:00:00Z",
        "Europe/London",
        Resolution::Day,
    );
    let starts: Vec<Wall> = spring.buckets.iter().map(|bucket| bucket.start).collect();
    // 29 March begins at midnight GMT, and 30 March at midnight BST.
    assert_eq!(
        starts,
        [wall("2026-03-29T00:00:00Z"), wall("2026-03-29T23:00:00Z")]
    );
    assert_eq!(spring.buckets[0].turns, 2);

    record(
        folder.path(),
        APP,
        [
            // 25 October 2026: 00:30 BST, then 23:30 GMT, one day of 25
            // hours, as the clocks go back at 01:00 GMT.
            line("2026-10-24T23:30:00Z", 2, 1),
            line("2026-10-25T23:30:00Z", 2, 2),
            line("2026-10-26T00:30:00Z", 2, 3),
        ],
    );
    let autumn = summary(
        folder.path(),
        "2026-10-01T00:00:00Z",
        "2026-11-01T00:00:00Z",
        "Europe/London",
        Resolution::Day,
    );
    let starts: Vec<(Wall, u64)> = autumn
        .buckets
        .iter()
        .map(|bucket| (bucket.start, bucket.turns))
        .collect();
    assert_eq!(
        starts,
        [
            (wall("2026-10-24T23:00:00Z"), 2),
            (wall("2026-10-26T00:00:00Z"), 1),
        ]
    );
}

#[test]
fn hours_begin_in_the_querys_time_zone() {
    let folder = Folder::new("hours");
    record(
        folder.path(),
        APP,
        [
            // 01:30 BST, then 01:30 GMT: the hour the clocks go back is
            // two hours.
            line("2026-10-25T00:30:00Z", 1, 1),
            line("2026-10-25T01:30:00Z", 1, 2),
            line("2026-10-25T01:59:59Z", 1, 3),
        ],
    );
    let london = summary(
        folder.path(),
        "2026-10-25T00:00:00Z",
        "2026-10-26T00:00:00Z",
        "Europe/London",
        Resolution::Hour,
    );
    let starts: Vec<(Wall, u64)> = london
        .buckets
        .iter()
        .map(|bucket| (bucket.start, bucket.turns))
        .collect();
    assert_eq!(
        starts,
        [
            (wall("2026-10-25T00:00:00Z"), 1),
            (wall("2026-10-25T01:00:00Z"), 2),
        ]
    );

    // Half an hour off UTC: 15:40 in Kolkata is in the hour from 15:00,
    // 09:30 UTC.
    record(folder.path(), APP, [line("2026-11-22T10:10:00Z", 2, 1)]);
    let kolkata = summary(
        folder.path(),
        "2026-11-22T00:00:00Z",
        "2026-11-23T00:00:00Z",
        "Asia/Kolkata",
        Resolution::Hour,
    );
    assert_eq!(kolkata.buckets[0].start, wall("2026-11-22T09:30:00Z"));
}

#[test]
fn a_period_is_from_since_until_until() {
    let folder = Folder::new("period");
    record(
        folder.path(),
        APP,
        [
            line("2026-11-22T09:59:59.999Z", 1, 1),
            line("2026-11-22T10:00:00Z", 1, 2),
            line("2026-11-22T10:59:59.999Z", 1, 3),
            line("2026-11-22T11:00:00Z", 1, 4),
        ],
    );
    let hour = summary(
        folder.path(),
        "2026-11-22T10:00:00Z",
        "2026-11-22T11:00:00Z",
        "UTC",
        Resolution::Hour,
    );
    assert_eq!(turns(&hour), 2);
}

#[test]
fn buckets_are_by_app_agent_model_and_fast_mode_sorted() {
    let folder = Folder::new("buckets");
    let mut fast = line("2026-11-22T10:00:00Z", 1, 2);
    fast["usage"]["fast"] = json!(true);
    let mut two = line("2026-11-22T10:00:00Z", 1, 3);
    two["usage"]["models"] = json!([
        {"model": "claude-opus-5-5", "tokens": {"input": 1, "cache_read": 0, "cache_write": 0, "output": 1}},
        {"model": "claude-haiku-5-5", "tokens": {"input": 2, "cache_read": 0, "cache_write": 0, "output": 2}},
    ]);
    let mut profiled = line("2026-11-22T10:00:00Z", 2, 1);
    profiled["profile"] = json!("claude-agent-acp");
    record(
        folder.path(),
        APP,
        [line("2026-11-23T10:00:00Z", 1, 1), fast, two, profiled],
    );
    let mut theirs = line("2026-11-22T10:00:00Z", 3, 1);
    theirs["app"] = json!("eyespy");
    record(folder.path(), "eyespy", [theirs]);

    let summary = november(folder.path());
    let keys: Vec<(Wall, &str, &str, &str, bool, u64)> = summary
        .buckets
        .iter()
        .map(|bucket| {
            (
                bucket.start,
                bucket.app.as_str(),
                bucket.agent.as_str(),
                bucket.model.as_str(),
                bucket.fast,
                bucket.turns,
            )
        })
        .collect();
    let day = wall("2026-11-22T00:00:00Z");
    let next = wall("2026-11-23T00:00:00Z");
    assert_eq!(
        keys,
        [
            (day, "clackatoo", "claude", "claude-haiku-5-5", false, 1),
            (day, "clackatoo", "claude", "claude-haiku-5-5", true, 1),
            (day, "clackatoo", "claude", "claude-opus-5-5", false, 1),
            (
                day,
                "clackatoo",
                "claude-agent-acp",
                "claude-haiku-5-5",
                false,
                1
            ),
            (day, "eyespy", "claude", "claude-haiku-5-5", false, 1),
            (next, "clackatoo", "claude", "claude-haiku-5-5", false, 1),
        ]
    );
    // A turn on two models is a turn in each.
    assert_eq!(summary.buckets[0].tokens.input, 2);
    assert_eq!(summary.fetched, Some(Wall(42)));
    let priced: Vec<&str> = summary
        .prices
        .iter()
        .map(|priced| priced.model.as_str())
        .collect();
    assert_eq!(priced, ["anthropic/claude-haiku-5-5", "claude-opus-5-5"]);
}

#[test]
fn only_the_apps_and_agents_asked_for() {
    let folder = Folder::new("filters");
    let mut profiled = line("2026-11-22T10:00:00Z", 2, 1);
    profiled["profile"] = json!("claude-agent-acp");
    record(
        folder.path(),
        APP,
        [line("2026-11-22T10:00:00Z", 1, 1), profiled],
    );
    let mut theirs = line("2026-11-22T10:00:00Z", 3, 1);
    theirs["app"] = json!("eyespy");
    theirs["profile"] = json!("claude-agent-acp");
    record(folder.path(), "eyespy", [theirs]);

    let mut query = UsageQuery::new(
        wall("2026-11-01T00:00:00Z"),
        wall("2026-12-01T00:00:00Z"),
        "UTC",
    );
    query.apps = vec!["eyespy".into()];
    let eyespy = summarise(folder.path(), &query, &prices()).expect("it's good");
    assert_eq!(eyespy.chats.all, 1);
    assert_eq!(eyespy.buckets[0].app, "eyespy");

    query.apps = Vec::new();
    // By profile, where there's one: the app's name for it doesn't count.
    query.agents = vec!["claude-agent-acp".into()];
    let profile = summarise(folder.path(), &query, &prices()).expect("it's good");
    assert_eq!(profile.chats.all, 2);
    query.agents = vec!["claude".into()];
    let named = summarise(folder.path(), &query, &prices()).expect("it's good");
    assert_eq!(named.chats.all, 1);
    assert_eq!(named.buckets[0].agent, "claude");
}

#[test]
fn turns_with_no_reported_cost_are_estimated() {
    let folder = Folder::new("estimated");
    let mut fast = line("2026-11-22T10:00:00Z", 1, 1);
    fast["usage"]["fast"] = json!(true);
    fast["usage"]["models"] = json!([
        {"model": "claude-opus-5-5", "tokens": {"input": 1_000_000, "cache_read": 0, "cache_write": 0, "output": 0}},
    ]);
    record(
        folder.path(),
        APP,
        [line("2026-11-22T10:00:00Z", 1, 2), fast],
    );
    let summary = november(folder.path());
    // Haiku: a thousand in at $1 and out at $5 a million.
    let haiku = &summary.buckets[0];
    near(haiku.estimated, 0.006);
    near(haiku.reported, 0.0);
    // Opus in fast mode: twice $5.
    let opus = &summary.buckets[1];
    assert!(opus.fast);
    near(opus.estimated, 10.0);
}

#[test]
fn a_reported_cost_is_shared_by_the_models_estimates() {
    let folder = Folder::new("reported");
    let mut turn = line("2026-11-22T10:00:00Z", 1, 1);
    turn["usage"]["cost"] = json!({"amount": 2.0, "currency": "USD"});
    // $1 and $3 at the table's prices.
    turn["usage"]["models"] = json!([
        {"model": "claude-haiku-5-5", "tokens": {"input": 1_000_000, "cache_read": 0, "cache_write": 0, "output": 0}},
        {"model": "claude-opus-5-5", "tokens": {"input": 600_000, "cache_read": 0, "cache_write": 0, "output": 0}},
        {"model": "mystery", "tokens": {"input": 7, "cache_read": 0, "cache_write": 0, "output": 0}},
    ]);
    record(folder.path(), APP, [turn]);
    let summary = november(folder.path());
    let by_model = |model: &str| -> &Bucket {
        summary
            .buckets
            .iter()
            .find(|bucket| bucket.model == model)
            .expect("it's there")
    };
    near(by_model("claude-haiku-5-5").reported, 0.5);
    near(by_model("claude-opus-5-5").reported, 1.5);
    near(by_model("mystery").reported, 0.0);
    for bucket in &summary.buckets {
        near(bucket.estimated, 0.0);
        assert_eq!(bucket.unpriced, 0, "a reported turn has nothing unpriced");
    }
}

#[test]
fn a_reported_cost_on_unpriced_models_is_shared_by_their_tokens() {
    let folder = Folder::new("reported-unpriced");
    let mut turn = line("2026-11-22T10:00:00Z", 1, 1);
    turn["usage"]["cost"] = json!({"amount": 1.0, "currency": "USD"});
    turn["usage"]["models"] = json!([
        {"model": "one", "tokens": {"input": 10, "cache_read": 0, "cache_write": 0, "output": 30}},
        {"model": "two", "tokens": {"input": 0, "cache_read": 60, "cache_write": 0, "output": 0}},
    ]);
    let mut empty = line("2026-11-22T10:00:00Z", 1, 2);
    empty["usage"]["cost"] = json!({"amount": 1.0, "currency": "USD"});
    empty["usage"]["models"] = json!([
        {"model": "three", "tokens": {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0}},
        {"model": "four", "tokens": {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0}},
    ]);
    record(folder.path(), APP, [turn, empty]);
    let summary = november(folder.path());
    let reported = |model: &str| {
        summary
            .buckets
            .iter()
            .find(|bucket| bucket.model == model)
            .expect("it's there")
            .reported
    };
    near(reported("one"), 0.4);
    near(reported("two"), 0.6);
    // No tokens either: evenly.
    near(reported("three"), 0.5);
    near(reported("four"), 0.5);
}

#[test]
fn a_cost_in_another_currency_is_estimated() {
    let folder = Folder::new("currency");
    let mut turn = line("2026-11-22T10:00:00Z", 1, 1);
    turn["usage"]["cost"] = json!({"amount": 9.0, "currency": "EUR"});
    record(folder.path(), APP, [turn]);
    let bucket = &november(folder.path()).buckets[0];
    near(bucket.reported, 0.0);
    near(bucket.estimated, 0.006);
}

#[test]
fn tokens_no_price_covers_are_unpriced() {
    let folder = Folder::new("unpriced");
    let mut turn = line("2026-11-22T10:00:00Z", 1, 1);
    turn["usage"]["models"][0]["model"] = json!("mystery");
    record(
        folder.path(),
        APP,
        [turn, line("2026-11-22T10:00:00Z", 1, 2)],
    );
    let summary = november(folder.path());
    let mystery = summary
        .buckets
        .iter()
        .find(|bucket| bucket.model == "mystery")
        .expect("it's there");
    assert_eq!(mystery.unpriced, 2000);
    near(mystery.estimated, 0.0);
    // Only models with a price are listed.
    assert_eq!(summary.prices.len(), 1);
}

#[test]
fn chats_are_counted_once_each_overall_per_app_and_per_agent() {
    let folder = Folder::new("chats");
    let mut profiled = line("2026-11-23T10:00:00Z", 1, 2);
    profiled["profile"] = json!("claude-agent-acp");
    let mut opus = line("2026-11-22T10:00:00Z", 2, 1);
    opus["usage"]["models"][0]["model"] = json!("claude-opus-5-5");
    record(
        folder.path(),
        APP,
        [
            line("2026-11-22T10:00:00Z", 1, 1),
            profiled,
            opus,
            line("2026-11-24T10:00:00Z", 2, 2),
        ],
    );
    let mut theirs = line("2026-11-22T10:00:00Z", 3, 1);
    theirs["app"] = json!("eyespy");
    record(folder.path(), "eyespy", [theirs]);

    let chats = november(folder.path()).chats;
    assert_eq!(chats.all, 3);
    assert_eq!(
        chats.by_app,
        [("clackatoo".to_owned(), 2), ("eyespy".to_owned(), 1)]
    );
    assert_eq!(
        chats.by_agent,
        [("claude".to_owned(), 3), ("claude-agent-acp".to_owned(), 1)]
    );
}

#[test]
fn a_query_that_cant_be_answered_is_refused() {
    let nowhere = Path::new("/nonexistent/caf-usage");
    let since = wall("2026-11-01T00:00:00Z");
    let query = |until: Wall, zone: &str| UsageQuery::new(since, until, zone);
    assert_eq!(
        summarise(
            nowhere,
            &query(wall("2026-11-02T00:00:00Z"), "Mars/Olympus"),
            &prices()
        ),
        Err(BadQuery::Zone("Mars/Olympus".into()))
    );
    for until in [
        since,
        Wall(since.0 - 1),
        Wall(since.0 + 400 * 86_400_000 + 1),
    ] {
        assert_eq!(
            summarise(nowhere, &query(until, "UTC"), &prices()),
            Err(BadQuery::Period)
        );
    }
    let longest = summarise(
        nowhere,
        &query(Wall(since.0 + 400 * 86_400_000), "UTC"),
        &prices(),
    )
    .expect("400 days is fine");
    assert!(longest.buckets.is_empty());
    assert!(longest.unreadable.is_empty());
}

#[test]
fn a_ledger_not_there_yet_has_no_usage() {
    let folder = Folder::new("empty");
    let summary = november(folder.path());
    assert!(summary.buckets.is_empty());
    assert_eq!(summary.chats.all, 0);
    assert!(summary.unreadable.is_empty());
    assert!(!folder.path().exists(), "reading makes nothing");
}

#[test]
fn an_app_named_as_no_folder_can_be_is_refused() {
    let folder = Folder::new("names");
    for app in [
        "", "prices", "Prices", ".hidden", "a/b", "a\\b", "a\0b", "..", ".",
    ] {
        let error = Ledger::new(folder.path().to_owned(), app)
            .record(&read(line("2026-11-22T10:00:00Z", 1, 1)))
            .expect_err("it's refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{app:?}");
    }
    assert!(!folder.path().exists());
}

#[test]
fn the_default_folder_is_beside_the_agents_caf_installs() {
    let os = |text: &str| Some(OsString::from(text));
    assert_eq!(
        folder_from(os("/data"), os("/home/me")),
        Some(PathBuf::from("/data/caf/usage"))
    );
    assert_eq!(
        folder_from(None, os("/home/me")),
        Some(PathBuf::from("/home/me/.local/share/caf/usage"))
    );
    // Only absolute paths, as the XDG rules say.
    assert_eq!(
        folder_from(os("data"), os("/home/me")),
        Some(PathBuf::from("/home/me/.local/share/caf/usage"))
    );
    assert_eq!(folder_from(os("data"), os("home")), None);
    assert_eq!(folder_from(None, None), None);
}

/// Writes `bytes` to `app`'s file `name`, making its folder.
fn put(folder: &Path, app: &str, name: &str, bytes: &[u8]) {
    let app = folder.join(app);
    std::fs::create_dir_all(&app).expect("it's made");
    std::fs::write(app.join(name), bytes).expect("it's written");
}

/// A line's JSON as written, with its line end.
fn text(value: &Value) -> String {
    format!("{value}\n")
}

#[test]
fn an_unreadable_last_line_with_its_end_names_the_file() {
    let folder = Folder::new("garbage-last");
    let good = text(&line("2026-11-22T10:00:00Z", 1, 1));
    put(
        folder.path(),
        APP,
        "2026-11.jsonl",
        format!("{good}GARBAGE\n").as_bytes(),
    );
    let packed = zstd::encode_all(format!("{good}GARBAGE\n").as_bytes(), 3).expect("it packs");
    put(folder.path(), "eyespy", "2026-11.jsonl.zst", &packed);

    let summary = november(folder.path());
    assert_eq!(turns(&summary), 1);
    assert_eq!(
        summary.unreadable,
        ["clackatoo/2026-11.jsonl", "eyespy/2026-11.jsonl.zst"]
    );
}

#[test]
fn a_last_line_without_its_end_never_counts() {
    let folder = Folder::new("unended");
    let good = text(&line("2026-11-22T10:00:00Z", 1, 1));
    let unended = line("2026-11-22T11:00:00Z", 1, 2).to_string();
    put(
        folder.path(),
        APP,
        "2026-11.jsonl",
        format!("{good}{unended}").as_bytes(),
    );
    let before = november(folder.path());
    assert_eq!(turns(&before), 1, "a line not ended may yet be cut off");
    assert!(before.unreadable.is_empty());

    // So the next line, written over it, takes nothing away.
    record(folder.path(), APP, [line("2026-11-22T12:00:00Z", 1, 3)]);
    assert_eq!(turns(&november(folder.path())), 2);
}

#[test]
fn a_failed_compression_doesnt_stop_the_line() {
    let folder = Folder::new("compress-fails");
    let october = text(&line("2026-10-30T10:00:00Z", 1, 1));
    put(folder.path(), APP, "2026-10.jsonl", october.as_bytes());
    put(folder.path(), APP, "2026-10.jsonl.zst", b"not zstd");

    let ledger = Ledger::new(folder.path().to_owned(), APP);
    ledger
        .record(&read(line("2026-11-22T10:00:00Z", 1, 2)))
        .expect("the line is written all the same");
    assert_eq!(turns(&november(folder.path())), 1);
    let left = std::fs::read_to_string(file(folder.path(), APP, "2026-10")).expect("it's there");
    assert_eq!(left, october, "October's lines are as they were");

    // Not tried again by this ledger, even once it could be.
    std::fs::remove_file(folder.path().join(APP).join("2026-10.jsonl.zst")).expect("it's gone");
    ledger
        .record(&read(line("2026-11-22T11:00:00Z", 1, 3)))
        .expect("it's written");
    assert!(file(folder.path(), APP, "2026-10").exists());
    // A new one, as after a restart, does.
    record(folder.path(), APP, [line("2026-11-22T12:00:00Z", 1, 4)]);
    assert_eq!(
        names(folder.path(), APP),
        ["2026-10.jsonl.zst", "2026-11.jsonl"]
    );
}

#[test]
fn a_line_with_a_count_past_2_to_the_53_is_unreadable() {
    let folder = Folder::new("too-big");
    let mut big = line("2026-11-22T10:00:00Z", 1, 1);
    big["usage"]["models"][0]["tokens"]["output"] = json!(1_u64 << 53);
    let mut thinking = line("2026-11-22T10:00:00Z", 1, 2);
    thinking["usage"]["models"][0]["tokens"]["thinking"] = json!(1_u64 << 53);
    let good = line("2026-11-22T10:00:00Z", 1, 3);
    put(
        folder.path(),
        APP,
        "2026-11.jsonl",
        format!("{}{}{}", text(&big), text(&thinking), text(&good)).as_bytes(),
    );
    let summary = november(folder.path());
    assert_eq!(turns(&summary), 1);
    assert_eq!(summary.unreadable, ["clackatoo/2026-11.jsonl"]);

    // Nor is one written.
    let error = Ledger::new(folder.path().to_owned(), "eyespy")
        .record(&read({
            let mut big = big.clone();
            big["app"] = json!("eyespy");
            big
        }))
        .expect_err("it's refused");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn sums_stop_below_2_to_the_53() {
    let folder = Folder::new("sums");
    let most = (1_u64 << 53) - 1;
    let lines = (1..=3).map(|turn| {
        let mut line = line("2026-11-22T10:00:00Z", 1, turn);
        line["usage"]["models"][0]["model"] = json!("mystery");
        line["usage"]["models"][0]["tokens"]["input"] = json!(most);
        line
    });
    record(folder.path(), APP, lines);
    let bucket = &november(folder.path()).buckets[0];
    assert_eq!(bucket.tokens.input, most);
    assert_eq!(bucket.unpriced, most);
    assert_eq!(bucket.turns, 3);
}

#[test]
fn a_line_another_apps_or_with_a_cost_not_a_number_is_refused() {
    let folder = Folder::new("refused");
    let ledger = Ledger::new(folder.path().to_owned(), "eyespy");
    let error = ledger
        .record(&read(line("2026-11-22T10:00:00Z", 1, 1)))
        .expect_err("clackatoo's isn't eyespy's to write");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

    let mut costly = line("2026-11-22T10:00:00Z", 1, 1);
    costly["app"] = json!("eyespy");
    costly["usage"]["cost"] = json!({"amount": 1.0, "currency": "USD"});
    for amount in [f64::NAN, f64::INFINITY] {
        let mut line = read(costly.clone());
        if let Some(cost) = line.usage.cost.as_mut() {
            cost.amount = amount;
        }
        let error = ledger.record(&line).expect_err("it's refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
    assert!(!folder.path().exists());
}

#[test]
fn apps_not_asked_for_arent_read() {
    let folder = Folder::new("unasked");
    record(folder.path(), APP, [line("2026-11-22T10:00:00Z", 1, 1)]);
    put(folder.path(), "eyespy", "2026-11.jsonl.zst", b"not zstd");
    let mut query = UsageQuery::new(
        wall("2026-11-01T00:00:00Z"),
        wall("2026-12-01T00:00:00Z"),
        "UTC",
    );
    query.apps = vec![APP.into()];
    let summary = summarise(folder.path(), &query, &prices()).expect("it's good");
    assert_eq!(turns(&summary), 1);
    assert!(summary.unreadable.is_empty());
}

#[test]
fn the_compressed_line_counts_over_a_later_one_and_joining_drops_it() {
    let folder = Folder::new("join");
    record(
        folder.path(),
        APP,
        [
            line("2026-10-30T10:00:00Z", 1, 1),
            line("2026-11-01T10:00:00Z", 2, 1),
        ],
    );
    // The same turn again, late, with other tokens.
    let mut again = line("2026-10-30T10:00:00Z", 1, 1);
    again["usage"]["models"][0]["tokens"]["input"] = json!(5);
    record(
        folder.path(),
        APP,
        [again, line("2026-10-30T11:00:00Z", 1, 2)],
    );
    let october = |folder: &Path| {
        summary(
            folder,
            "2026-10-01T00:00:00Z",
            "2026-11-01T00:00:00Z",
            "UTC",
            Resolution::Day,
        )
    };
    let before = october(folder.path());
    assert_eq!(turns(&before), 2);
    assert_eq!(before.buckets[0].tokens.input, 2000);

    record(folder.path(), APP, [line("2026-12-01T10:00:00Z", 3, 1)]);
    assert_eq!(october(folder.path()), before);
    let packed =
        std::fs::read(folder.path().join(APP).join("2026-10.jsonl.zst")).expect("it's there");
    let lines = zstd::decode_all(packed.as_slice()).expect("it's zstd");
    assert_eq!(
        lines.iter().filter(|byte| **byte == b'\n').count(),
        2,
        "the duplicate isn't kept"
    );
}

#[test]
fn a_cost_in_dollars_is_reported_however_its_currency_is_written() {
    let folder = Folder::new("usd");
    let mut turn = line("2026-11-22T10:00:00Z", 1, 1);
    turn["usage"]["cost"] = json!({"amount": 0.25, "currency": "usd"});
    record(folder.path(), APP, [turn]);
    let bucket = &november(folder.path()).buckets[0];
    near(bucket.reported, 0.25);
    near(bucket.estimated, 0.0);
}

#[test]
fn a_zone_ahead_of_utc_reads_across_the_month() {
    let folder = Folder::new("ahead");
    record(
        folder.path(),
        APP,
        [
            // 1 December, 01:00 in Auckland, but November in UTC.
            line("2026-11-30T12:00:00Z", 1, 1),
            line("2026-12-01T12:00:00Z", 1, 2),
        ],
    );
    let summary = summary(
        folder.path(),
        "2026-11-30T11:00:00Z",
        "2026-12-01T11:00:00Z",
        "Pacific/Auckland",
        Resolution::Day,
    );
    let starts: Vec<(Wall, u64)> = summary
        .buckets
        .iter()
        .map(|bucket| (bucket.start, bucket.turns))
        .collect();
    assert_eq!(starts, [(wall("2026-11-30T11:00:00Z"), 1)]);
}

#[test]
fn a_ledger_that_cant_be_listed_is_named() {
    use std::os::unix::fs::PermissionsExt;

    if rustix::process::geteuid().is_root() {
        // Root lists anything.
        return;
    }
    let folder = Folder::new("unlistable");
    record(folder.path(), APP, [line("2026-11-22T10:00:00Z", 1, 1)]);
    let set = |mode| {
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(mode))
            .expect("it's set");
    };
    set(0o000);
    let summary = november(folder.path());
    set(0o700);
    assert!(summary.buckets.is_empty());
    assert_eq!(summary.unreadable, ["."]);
}

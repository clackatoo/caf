//! Tests of the account limits kept beside the ledger.

use std::path::{Path, PathBuf};
use std::time::Duration;

use caf_core::limits::{AccountLimits, ProfileLimits, WindowKind};
use caf_core::profiles::Builtin;
use caf_core::time::Wall;
use caf_core::usage::{PriceTable, UsageQuery};
use serde_json::{Value, json};

use super::{load, save};
use crate::testing::Folder;

/// The profile the tests keep limits for.
const PROFILE: Builtin = Builtin::ClaudeAgentAcp;
/// An hour, in milliseconds.
const HOUR: i64 = 3_600_000;
/// When the tests save and load: 2026-11-22T10:00:00Z.
const NOW: Wall = Wall(1_795_341_600_000);

/// `hours` hours from now, in milliseconds.
fn at(hours: i64) -> i64 {
    NOW.0 + hours * HOUR
}

/// A window's JSON: of `kind`, `used`, resetting at `resets`, heard at
/// `heard`, still taking turns.
fn window(kind: &Value, used: u8, resets: Option<i64>, heard: i64) -> Value {
    json!({
        "kind": kind,
        "used": used,
        "resets": resets,
        "state": {"type": "Fine"},
        "heard": heard,
    })
}

/// The five hours, `used`, heard at `heard`, resetting in four hours.
fn five_hours(used: u8, heard: i64) -> Value {
    window(&json!({"type": "FiveHours"}), used, Some(at(4)), heard)
}

/// A model's week, `used`, heard at `heard`, resetting in a day.
fn model_week(model: &str, used: u8, heard: i64) -> Value {
    window(
        &json!({"type": "ModelWeek", "value": model}),
        used,
        Some(at(24)),
        heard,
    )
}

/// Limits of `windows`, cleared at `cleared`, if ever.
fn limits(windows: Vec<Value>, cleared: Option<i64>) -> AccountLimits {
    let limits = json!({"windows": windows, "extra_usage": null, "cleared": cleared});
    serde_json::from_value(limits).expect("the test's limits read")
}

/// Those limits, as the profile's.
fn profile(limits: AccountLimits) -> ProfileLimits {
    ProfileLimits::new(PROFILE, limits)
}

/// The profile's file in `folder`.
fn file(folder: &Path) -> PathBuf {
    folder.join("limits").join("claude-agent-acp.json")
}

/// What's kept of the profile's limits in `folder`, now, which must read.
fn kept(folder: &Path) -> AccountLimits {
    let (kept, errors) = load(folder, NOW);
    assert!(errors.is_empty(), "{errors:?}");
    match kept.as_slice() {
        [] => AccountLimits::default(),
        [kept] if kept.profile == PROFILE => kept.limits.clone(),
        kept => panic!("one profile's: {kept:?}"),
    }
}

/// Saves `limits` as the profile's in `folder`, now.
fn saved(folder: &Path, limits: AccountLimits) -> Option<AccountLimits> {
    save(folder, &profile(limits), NOW).expect("it's saved")
}

#[test]
fn limits_saved_are_loaded_as_their_profiles() {
    let folder = Folder::new("limits-saved");
    let given = limits(
        vec![five_hours(40, at(-1)), model_week("opus", 7, at(-1))],
        None,
    );
    assert_eq!(
        saved(folder.path(), given.clone()),
        None,
        "nothing new kept"
    );
    assert_eq!(load(folder.path(), NOW).0, [profile(given.clone())]);
    let text = std::fs::read(file(folder.path())).expect("it's there");
    assert_eq!(
        serde_json::from_slice::<AccountLimits>(&text).expect("JSON in CAF's one shape"),
        given
    );
}

#[test]
fn a_save_takes_in_what_was_kept_and_says_what_it_had() {
    let folder = Folder::new("limits-merged");
    saved(folder.path(), limits(vec![five_hours(40, at(-1))], None));
    let news = json!({
        "windows": [model_week("opus", 7, at(-2))],
        "extra_usage": {"in_use": true, "heard": at(-2)},
        "cleared": null,
    });
    let news: AccountLimits = serde_json::from_value(news).unwrap();
    let merged = saved(folder.path(), news.clone()).expect("the five hours were news");
    let mut expected = limits(vec![five_hours(40, at(-1))], None);
    expected.merge(&news);
    assert_eq!(merged, expected);
    assert!(merged.window(&WindowKind::FiveHours).is_some());
    assert!(
        merged
            .window(&WindowKind::ModelWeek("opus".into()))
            .is_some()
    );
    assert!(merged.extra_usage.is_some_and(|extra| extra.in_use));
    assert_eq!(kept(folder.path()), merged, "and kept");
}

#[test]
fn a_save_never_undoes_later_news() {
    let folder = Folder::new("limits-later");
    saved(folder.path(), limits(vec![five_hours(40, at(-1))], None));
    // Older news, as another app that heard it first saves it late.
    let merged = saved(folder.path(), limits(vec![five_hours(20, at(-2))], None));
    let later = limits(vec![five_hours(40, at(-1))], None);
    assert_eq!(merged, Some(later.clone()), "the later is told");
    assert_eq!(kept(folder.path()), later);
    // Later news replaces it.
    let latest = limits(vec![five_hours(60, at(0))], None);
    assert_eq!(saved(folder.path(), latest.clone()), None);
    assert_eq!(kept(folder.path()), latest);
}

#[test]
fn two_writers_at_once_both_count() {
    let folder = Folder::new("limits-threads");
    std::thread::scope(|scope| {
        for (writer, model) in ["opus", "sonnet", "fable"].into_iter().enumerate() {
            let path = folder.path();
            scope.spawn(move || {
                for step in 0..40 {
                    let heard = at(-1) + step * 3 + i64::try_from(writer).unwrap();
                    let windows = vec![five_hours(10, heard), model_week(model, 5, heard)];
                    save(path, &profile(limits(windows, None)), NOW).expect("it's saved");
                }
            });
        }
    });
    let kept = kept(folder.path());
    let five = kept.window(&WindowKind::FiveHours).expect("the five hours");
    assert_eq!(five.heard, Wall(at(-1) + 39 * 3 + 2), "the latest heard");
    for (writer, model) in ["opus", "sonnet", "fable"].into_iter().enumerate() {
        let week = kept.window(&WindowKind::ModelWeek(model.into()));
        let heard = at(-1) + 39 * 3 + i64::try_from(writer).unwrap();
        assert_eq!(week.map(|week| week.heard), Some(Wall(heard)), "{model}");
    }
}

/// The writer the process test runs in each child: set by
/// `CAF_LIMITS_WRITER` as `<folder>|<writer>|<model>`. Saves the five hours
/// and its model's week 60 times, each heard later than the last, its
/// five hours a moment apart from the other writer's.
#[test]
#[ignore = "a child of two_programs_saving_at_once_both_count"]
fn writer() {
    let Some(task) = std::env::var_os("CAF_LIMITS_WRITER") else {
        return;
    };
    let task = task.into_string().expect("text");
    let mut parts = task.split('|');
    let (Some(folder), Some(writer), Some(model)) = (parts.next(), parts.next(), parts.next())
    else {
        panic!("folder|writer|model");
    };
    let writer: i64 = writer.parse().expect("a number");
    for step in 0..60 {
        let heard = at(-1) + step * 2 + writer;
        let windows = vec![five_hours(10, heard), model_week(model, 5, heard)];
        save(Path::new(folder), &profile(limits(windows, None)), NOW).expect("it's saved");
    }
}

#[test]
fn two_programs_saving_at_once_both_count() {
    let folder = Folder::new("limits-processes");
    let me = std::env::current_exe().expect("the test's program");
    let children: Vec<_> = [(0, "opus"), (1, "sonnet")]
        .into_iter()
        .map(|(writer, model)| {
            std::process::Command::new(&me)
                .args(["--exact", "limits::tests::writer", "--ignored", "--quiet"])
                .env(
                    "CAF_LIMITS_WRITER",
                    format!("{}|{writer}|{model}", folder.path().display()),
                )
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("it starts")
        })
        .map(crate::testing::Child)
        .collect();
    for mut child in children {
        assert!(child.0.wait().expect("it ends").success());
    }
    let kept = kept(folder.path());
    let five = kept.window(&WindowKind::FiveHours).expect("the five hours");
    assert_eq!(five.heard, Wall(at(-1) + 59 * 2 + 1), "the latest heard");
    for (writer, model) in [(0, "opus"), (1, "sonnet")] {
        let week = kept.window(&WindowKind::ModelWeek(model.into()));
        let heard = at(-1) + 59 * 2 + writer;
        assert_eq!(week.map(|week| week.heard), Some(Wall(heard)), "{model}");
    }
}

#[test]
fn a_window_past_its_reset_is_never_kept() {
    let folder = Folder::new("limits-passed");
    let passed = window(&json!({"type": "Week"}), 90, Some(at(-1)), at(-2));
    let resetting = window(&json!({"type": "ExtraWeek"}), 90, Some(NOW.0), at(-2));
    let given = limits(vec![five_hours(40, at(-1)), passed, resetting], None);
    assert_eq!(saved(folder.path(), given), None, "nothing new kept");
    let only = limits(vec![five_hours(40, at(-1))], None);
    assert_eq!(kept(folder.path()), only.clone());
    let text: AccountLimits =
        serde_json::from_slice(&std::fs::read(file(folder.path())).unwrap()).unwrap();
    assert_eq!(text, only, "nor written");

    // One kept that has since passed is dropped as it's read and saved.
    let later = Wall(at(5));
    let (read, errors) = load(folder.path(), later);
    assert!(errors.is_empty());
    assert_eq!(read, [profile(AccountLimits::default())]);
    let week = model_week("opus", 5, at(4));
    let merged = save(
        folder.path(),
        &profile(limits(vec![week.clone()], None)),
        later,
    );
    assert_eq!(merged.unwrap(), None, "a passed window is no news");
    let text: AccountLimits =
        serde_json::from_slice(&std::fs::read(file(folder.path())).unwrap()).unwrap();
    assert_eq!(text, limits(vec![week], None));
}

#[test]
fn a_window_heard_before_a_clearing_is_never_kept() {
    let folder = Folder::new("limits-cleared");
    saved(folder.path(), limits(vec![five_hours(40, at(-2))], None));
    // Signed out of, an hour ago.
    let cleared = limits(Vec::new(), Some(at(-1)));
    assert_eq!(saved(folder.path(), cleared.clone()), None);
    assert_eq!(kept(folder.path()), cleared);
    // A copy from before is never taken back.
    let merged = saved(folder.path(), limits(vec![five_hours(40, at(-2))], None));
    assert_eq!(merged, Some(cleared.clone()), "the clearing is told");
    assert_eq!(kept(folder.path()), cleared);
    // A file holding one, written by hand, is read without it.
    let by_hand = json!({
        "windows": [five_hours(40, at(-2))],
        "extra_usage": {"in_use": true, "heard": at(-2)},
        "cleared": at(-1),
    });
    std::fs::write(file(folder.path()), by_hand.to_string()).unwrap();
    assert_eq!(kept(folder.path()), cleared);
}

#[test]
fn a_file_no_profile_is_named_after_is_left_alone() {
    let folder = Folder::new("limits-others");
    saved(folder.path(), limits(vec![five_hours(40, at(-1))], None));
    let limits_folder = folder.path().join("limits");
    let others = [
        ("nobody.json", "not limits"),
        ("notes.txt", "mine"),
        ("claude-agent-acp.json.new", "a save cut off"),
    ];
    for (name, text) in others {
        std::fs::write(limits_folder.join(name), text).unwrap();
    }
    let (read, errors) = load(folder.path(), NOW);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(read.len(), 1);
    saved(folder.path(), limits(vec![five_hours(50, at(0))], None));
    for (name, text) in others
        .into_iter()
        .filter(|(name, _)| !name.ends_with(".new"))
    {
        assert_eq!(
            std::fs::read_to_string(limits_folder.join(name)).unwrap(),
            text
        );
    }
}

#[test]
fn a_file_that_doesnt_read_is_named_and_written_over() {
    let folder = Folder::new("limits-unreadable");
    std::fs::create_dir_all(folder.path().join("limits")).unwrap();
    std::fs::write(file(folder.path()), "{\"windows\": [").unwrap();
    let (read, errors) = load(folder.path(), NOW);
    assert!(read.is_empty());
    let [error] = errors.as_slice() else {
        panic!("one error: {errors:?}");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    crate::testing::names_only(error, folder.path(), "limits/claude-agent-acp.json");
    let given = limits(vec![five_hours(40, at(-1))], None);
    assert_eq!(saved(folder.path(), given.clone()), None);
    assert_eq!(kept(folder.path()), given);
}

#[test]
fn a_folder_that_cant_be_listed_is_named() {
    let folder = Folder::new("limits-listed");
    std::fs::create_dir_all(folder.path()).unwrap();
    std::fs::write(folder.path().join("limits"), "in the way").unwrap();
    let (read, errors) = load(folder.path(), NOW);
    assert!(read.is_empty());
    let [error] = errors.as_slice() else {
        panic!("one error: {errors:?}");
    };
    crate::testing::names_only(error, folder.path(), "limits");
    let given = profile(limits(vec![five_hours(40, at(-1))], None));
    assert!(save(folder.path(), &given, NOW).is_err());
}

#[test]
fn loading_none_makes_nothing() {
    let folder = Folder::new("limits-none");
    let (read, errors) = load(folder.path(), NOW);
    assert!(read.is_empty() && errors.is_empty());
    assert!(!folder.path().exists());
}

#[test]
fn a_summary_never_reads_the_limits_as_an_app() {
    let folder = Folder::new("limits-summary");
    saved(folder.path(), limits(vec![five_hours(40, at(-1))], None));
    let query = UsageQuery::new(Wall(at(-24)), Wall(at(24)), "UTC");
    let summary = crate::summarise(folder.path(), &query, &PriceTable::default()).unwrap();
    assert!(summary.unreadable.is_empty(), "{:?}", summary.unreadable);
    assert!(summary.buckets.is_empty());
}

#[test]
fn a_pipe_where_limits_should_be_never_hangs() {
    let folder = Folder::new("limits-pipe");
    let limits_folder = folder.path().join("limits");
    std::fs::create_dir_all(&limits_folder).unwrap();
    crate::testing::pipe(&file(folder.path()));
    let path = folder.path().to_owned();
    let limit = Duration::from_secs(10);
    let (read, tried) = crate::testing::within(limit, move || {
        let read = load(&path, NOW);
        let given = profile(limits(vec![five_hours(40, at(-1))], None));
        (read, save(&path, &given, NOW))
    });
    let (read, errors) = read;
    assert!(read.is_empty());
    let [error] = errors.as_slice() else {
        panic!("one error: {errors:?}");
    };
    crate::testing::names_only(error, folder.path(), "limits/claude-agent-acp.json");
    let error = tried.expect_err("a pipe isn't kept in");
    crate::testing::names_only(&error, folder.path(), "limits/claude-agent-acp.json");

    // Nor a pipe where the lock should be.
    std::fs::remove_file(file(folder.path())).unwrap();
    crate::testing::pipe(&limits_folder.join(".lock"));
    let path = folder.path().to_owned();
    let tried = crate::testing::within(limit, move || {
        let given = profile(limits(vec![five_hours(40, at(-1))], None));
        save(&path, &given, NOW)
    });
    let error = tried.expect_err("a pipe isn't a lock");
    crate::testing::names_only(&error, folder.path(), "limits/.lock");
}

#[test]
fn a_lock_held_by_another_is_given_up_on() {
    let folder = Folder::new("limits-held");
    let limits_folder = folder.path().join("limits");
    std::fs::create_dir_all(&limits_folder).unwrap();
    let held = crate::testing::hold(&limits_folder.join(".lock"));
    let path = folder.path().to_owned();
    let started = std::time::Instant::now();
    let tried = crate::testing::within(Duration::from_secs(15), move || {
        let given = profile(limits(vec![five_hours(40, at(-1))], None));
        save(&path, &given, NOW)
    });
    let took = started.elapsed();
    let error = tried.expect_err("it gives up");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    crate::testing::names_only(&error, folder.path(), "limits/.lock");
    assert!(took >= crate::files::LOCK_WAIT, "{took:?}");
    assert!(!file(folder.path()).exists());
    // Reading needs no lock.
    let (_, errors) = crate::testing::within(Duration::from_secs(5), {
        let path = folder.path().to_owned();
        move || load(&path, NOW)
    });
    assert!(errors.is_empty());
    drop(held);
    let given = limits(vec![five_hours(40, at(-1))], None);
    assert_eq!(saved(folder.path(), given.clone()), None);
    assert_eq!(kept(folder.path()), given);
}

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
    std::fs::remove_file(limits_folder.join(".lock")).unwrap();
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

/// The profile's file in `folder`, as JSON.
fn written(folder: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(file(folder)).expect("it's there")).expect("JSON")
}

/// Writes the profile's file in `folder` as `json`, as another CAF might.
fn write(folder: &Path, json: &Value) {
    std::fs::create_dir_all(folder.join("limits")).unwrap();
    std::fs::write(file(folder), json.to_string()).unwrap();
}

#[test]
fn windows_a_later_caf_wrote_are_skipped_and_kept_as_they_were() {
    let folder = Folder::new("limits-later-caf");
    let mut unknown_kind = window(&json!({"type": "Fortnight"}), 30, Some(at(24)), at(-1));
    unknown_kind["pace"] = json!("steady");
    let mut unknown_state = window(&json!({"type": "Week"}), 99, Some(at(24)), at(-1));
    unknown_state["state"] = json!({"type": "Throttled"});
    let unknown_extra = json!({"in_use": "partly", "heard": at(-1)});
    write(
        folder.path(),
        &json!({
            "windows": [five_hours(40, at(-1)), unknown_kind, unknown_state],
            "extra_usage": unknown_extra,
            "cleared": null,
        }),
    );
    assert_eq!(
        kept(folder.path()),
        limits(vec![five_hours(40, at(-1))], None),
        "the rest read"
    );

    let merged = saved(
        folder.path(),
        limits(vec![model_week("opus", 7, at(0))], None),
    );
    let read = limits(
        vec![five_hours(40, at(-1)), model_week("opus", 7, at(0))],
        None,
    );
    assert_eq!(merged, Some(read.clone()), "only what reads is news");
    let mut expected = serde_json::to_value(&read).unwrap();
    expected["windows"]
        .as_array_mut()
        .unwrap()
        .extend([unknown_kind, unknown_state]);
    expected["extra_usage"] = unknown_extra;
    assert_eq!(
        written(folder.path()),
        expected,
        "carried through as they were"
    );

    // Each goes as a window this CAF reads would: cleared after it was
    // heard, or its reset passed.
    let mut passed = window(&json!({"type": "Fortnight"}), 30, Some(at(-1)), at(-2));
    passed["state"] = json!({"type": "Throttled"});
    let mut later = expected.clone();
    later["windows"].as_array_mut().unwrap().push(passed);
    write(folder.path(), &later);
    saved(
        folder.path(),
        limits(vec![model_week("opus", 7, at(0))], None),
    );
    assert_eq!(written(folder.path()), expected, "past its reset");
    saved(folder.path(), limits(Vec::new(), Some(NOW.0 - 1)));
    assert_eq!(
        written(folder.path()),
        json!({"windows": [model_week("opus", 7, at(0))], "extra_usage": null, "cleared": NOW.0 - 1}),
        "heard before the clearing"
    );
}

#[test]
fn a_window_a_later_caf_wrote_goes_once_one_of_its_kind_is_heard_later() {
    let folder = Folder::new("limits-later-caf-kind");
    let mut throttled = five_hours(40, at(-1));
    throttled["state"] = json!({"type": "Throttled"});
    write(
        folder.path(),
        &json!({"windows": [throttled], "extra_usage": null, "cleared": null}),
    );
    let given = limits(vec![five_hours(50, at(0))], None);
    assert_eq!(saved(folder.path(), given.clone()), None);
    assert_eq!(
        written(folder.path()),
        serde_json::to_value(&given).unwrap()
    );
}

#[test]
fn a_file_far_bigger_than_limits_are_isnt_read() {
    let folder = Folder::new("limits-big");
    let mut big = limits(vec![five_hours(40, at(-1))], None);
    big.windows[0].kind = WindowKind::Other("x".repeat(2 << 20));
    write(folder.path(), &serde_json::to_value(&big).unwrap());
    let (read, errors) = load(folder.path(), NOW);
    assert!(read.is_empty());
    let [error] = errors.as_slice() else {
        panic!("one error: {errors:?}");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    crate::testing::names_only(error, folder.path(), "limits/claude-agent-acp.json");
    let given = limits(vec![five_hours(50, at(0))], None);
    assert_eq!(saved(folder.path(), given.clone()), None, "written over");
    assert_eq!(kept(folder.path()), given);
}

#[test]
fn a_time_from_a_clock_set_ahead_never_wins_for_long() {
    let folder = Folder::new("limits-clock");
    let ahead = at(24);
    write(
        folder.path(),
        &json!({
            "windows": [],
            "extra_usage": null,
            "cleared": ahead,
        }),
    );
    let read = kept(folder.path());
    let cleared = Wall(NOW.0 + super::CLEARED_SLACK);
    assert_eq!(
        read.cleared,
        Some(cleared),
        "no later than now, near enough"
    );
    let slack = Wall(NOW.0 + super::SLACK);

    write(
        folder.path(),
        &json!({"windows": [five_hours(90, ahead)], "extra_usage": {"in_use": true, "heard": ahead}, "cleared": null}),
    );
    let read = kept(folder.path());
    assert_eq!(read.windows[0].heard, slack);
    assert_eq!(read.extra_usage.map(|extra| extra.heard), Some(slack));
    // Kept as of then, so news heard after it wins.
    saved(folder.path(), limits(vec![five_hours(40, at(-1))], None));
    let then = NOW.0 + 2 * super::SLACK;
    let news = profile(limits(vec![five_hours(50, then)], None));
    save(folder.path(), &news, Wall(then)).unwrap();
    let (read, _) = load(folder.path(), Wall(then));
    let five = read[0].limits.window(&WindowKind::FiveHours).unwrap();
    assert_eq!((five.used, five.heard), (50, Wall(then)));
    // And a save's own times are held to it too.
    let ahead = profile(limits(vec![model_week("opus", 5, at(24))], None));
    save(folder.path(), &ahead, Wall(then)).unwrap();
    let (read, _) = load(folder.path(), Wall(then));
    let week = read[0].limits.window(&WindowKind::ModelWeek("opus".into()));
    assert_eq!(week.map(|week| week.heard), Some(Wall(then + super::SLACK)));
}

#[test]
fn a_save_that_changes_nothing_isnt_written() {
    use std::os::unix::fs::MetadataExt;
    let folder = Folder::new("limits-unchanged");
    let given = limits(vec![five_hours(40, at(-1))], None);
    saved(folder.path(), given.clone());
    let before = std::fs::metadata(file(folder.path())).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(saved(folder.path(), given.clone()), None);
    let older = limits(vec![five_hours(30, at(-2))], None);
    assert_eq!(saved(folder.path(), older), Some(given));
    let after = std::fs::metadata(file(folder.path())).unwrap();
    assert_eq!(
        (before.ino(), before.mtime_nsec()),
        (after.ino(), after.mtime_nsec())
    );
}

#[test]
fn a_cleared_only_file_is_loaded() {
    let folder = Folder::new("limits-cleared-only");
    saved(folder.path(), limits(Vec::new(), Some(at(-1))));
    assert_eq!(
        load(folder.path(), NOW).0,
        [profile(limits(Vec::new(), Some(at(-1))))]
    );
}

#[test]
fn extra_usage_a_later_caf_wrote_heard_later_is_kept_over_one_read() {
    let folder = Folder::new("limits-later-extra");
    let later = json!({"in_use": "partly", "heard": at(0)});
    write(
        folder.path(),
        &json!({"windows": [], "extra_usage": later, "cleared": null}),
    );
    let given = json!({
        "windows": [],
        "extra_usage": {"in_use": true, "heard": at(-1)},
        "cleared": null,
    });
    let given: AccountLimits = serde_json::from_value(given).unwrap();
    assert_eq!(
        saved(folder.path(), given.clone()),
        None,
        "nothing read is news"
    );
    assert_eq!(
        written(folder.path())["extra_usage"],
        later,
        "the later kept"
    );

    // One heard earlier goes.
    let earlier = json!({"in_use": "partly", "heard": at(-2)});
    write(
        folder.path(),
        &json!({"windows": [], "extra_usage": earlier, "cleared": null}),
    );
    saved(folder.path(), given.clone());
    assert_eq!(
        written(folder.path()),
        serde_json::to_value(&given).unwrap()
    );
}

#[test]
fn a_window_a_later_caf_wrote_heard_after_one_of_its_kind_is_kept_beside_it() {
    let folder = Folder::new("limits-later-caf-after");
    let mut throttled = five_hours(40, at(0));
    throttled["state"] = json!({"type": "Throttled"});
    write(
        folder.path(),
        &json!({"windows": [throttled], "extra_usage": null, "cleared": null}),
    );
    let given = limits(vec![five_hours(50, at(-1))], None);
    assert_eq!(saved(folder.path(), given.clone()), None);
    let mut expected = serde_json::to_value(&given).unwrap();
    expected["windows"].as_array_mut().unwrap().push(throttled);
    assert_eq!(written(folder.path()), expected);
}

#[test]
fn a_clearing_from_a_clock_set_ahead_never_drops_news_heard_since() {
    let folder = Folder::new("limits-clock-cleared");
    write(
        folder.path(),
        &json!({"windows": [], "extra_usage": null, "cleared": at(24)}),
    );
    let held = Wall(NOW.0 + super::CLEARED_SLACK);
    assert_eq!(
        kept(folder.path()).cleared,
        Some(held),
        "a few seconds from now"
    );
    // Kept as of then, by this app's next save.
    let cleared = limits(Vec::new(), Some(held.0));
    assert_eq!(
        saved(folder.path(), AccountLimits::default()),
        Some(cleared)
    );
    let news = limits(vec![five_hours(40, held.0 + 1000)], None);
    let merged = save(folder.path(), &profile(news), held).unwrap();
    let merged = merged.expect("the clearing is news");
    assert_eq!(merged.cleared, Some(held));
    assert_eq!(
        merged.window(&WindowKind::FiveHours).map(|five| five.used),
        Some(40),
        "kept"
    );
    let (read, _) = load(folder.path(), held);
    assert_eq!(read, [profile(merged)]);
}

#[test]
fn a_clearing_another_app_saved_while_this_one_waited_for_the_lock_stays() {
    let folder = Folder::new("limits-clock-waited");
    let soon = NOW.0 + 3000;
    write(
        folder.path(),
        &json!({"windows": [], "extra_usage": null, "cleared": soon}),
    );
    assert_eq!(kept(folder.path()).cleared, Some(Wall(soon)));
    // Heard before it, as this app's `now` was taken before the wait.
    let news = limits(vec![five_hours(40, NOW.0)], None);
    let merged = saved(folder.path(), news).expect("the clearing is news");
    assert_eq!(merged, limits(Vec::new(), Some(soon)), "dropped");
}

#[test]
fn what_this_caf_cant_read_heard_from_a_clock_set_ahead_is_held_to_now() {
    let folder = Folder::new("limits-clock-unread");
    let mut throttled = five_hours(40, at(24));
    throttled["state"] = json!({"type": "Throttled"});
    write(
        folder.path(),
        &json!({
            "windows": [throttled],
            "extra_usage": {"in_use": "partly", "heard": at(24)},
            "cleared": null,
        }),
    );
    let extra = |heard: i64| {
        let limits = json!({
            "windows": [],
            "extra_usage": {"in_use": true, "heard": heard},
            "cleared": null,
        });
        serde_json::from_value::<AccountLimits>(limits).unwrap()
    };
    saved(folder.path(), extra(at(-1)));
    let held = NOW.0 + super::SLACK;
    let file = written(folder.path());
    assert_eq!(
        file["extra_usage"],
        json!({"in_use": "partly", "heard": held})
    );
    assert_eq!(file["windows"][0]["heard"], json!(held));
    assert_eq!(file["windows"][0]["state"], json!({"type": "Throttled"}));

    // News heard two minutes later replaces each.
    let later = held + 120_000;
    let mut news = extra(later);
    news.merge(&limits(vec![five_hours(50, later)], None));
    save(folder.path(), &profile(news.clone()), Wall(later)).unwrap();
    assert_eq!(written(folder.path()), serde_json::to_value(&news).unwrap());
}

#[test]
fn a_file_written_over_is_a_warning_once_then_debugging() {
    let said = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(super::once_then_debug(&said), log::Level::Warn);
    assert_eq!(super::once_then_debug(&said), log::Level::Debug);
    assert_eq!(super::once_then_debug(&said), log::Level::Debug);
}

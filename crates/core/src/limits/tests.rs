//! Tests of account limits' arithmetic: windows' lengths and pace, and
//! merging, forgetting and the next reset.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::limits::{AccountLimits, ExtraUsage, LimitState, LimitWindow, WindowKind};
use crate::profiles::Builtin;
use crate::time::Wall;

/// An hour, in milliseconds.
const HOUR: i64 = 60 * 60 * 1000;

/// When the windows of these tests reset: 2026-11-18, 18:13:20 UTC.
const RESETS: i64 = 1_795_010_000_000;

/// A window of `kind`, `used`, resetting at `resets`, heard at `heard`.
fn window(kind: WindowKind, used: u8, resets: Option<i64>, heard: i64) -> LimitWindow {
    LimitWindow {
        kind,
        used,
        resets: resets.map(Wall),
        state: LimitState::Fine,
        heard: Wall(heard),
    }
}

/// Limits of these windows, and nothing else.
fn limits(windows: Vec<LimitWindow>) -> AccountLimits {
    AccountLimits {
        windows,
        extra_usage: None,
        cleared: None,
    }
}

/// Every kind of window, a model's week and another's included.
fn every_kind() -> Vec<WindowKind> {
    vec![
        WindowKind::FiveHours,
        WindowKind::Week,
        WindowKind::ModelWeek("opus".into()),
        WindowKind::ExtraWeek,
        WindowKind::Other("thirty_day".into()),
    ]
}

#[test]
fn a_window_is_five_hours_or_a_week_and_one_caf_doesnt_know_has_no_length() {
    let week = Some(Duration::from_secs(7 * 24 * 60 * 60));
    assert_eq!(
        WindowKind::FiveHours.length(),
        Some(Duration::from_secs(5 * 60 * 60))
    );
    assert_eq!(WindowKind::Week.length(), week);
    assert_eq!(WindowKind::ModelWeek("sonnet".into()).length(), week);
    assert_eq!(WindowKind::ExtraWeek.length(), week);
    assert_eq!(WindowKind::Other("five_hour_ish".into()).length(), None);
}

#[test]
fn pace_is_how_much_of_the_window_has_gone_kept_to_0_to_100() {
    let five = window(WindowKind::FiveHours, 50, Some(RESETS), RESETS - HOUR);
    let start = RESETS - 5 * HOUR;
    assert_eq!(five.pace(Wall(start - HOUR)), Some(0), "before it starts");
    assert_eq!(five.pace(Wall(start)), Some(0));
    assert_eq!(five.pace(Wall(start + HOUR)), Some(20));
    assert_eq!(five.pace(Wall(start + 5 * HOUR / 2)), Some(50));
    assert_eq!(five.pace(Wall(RESETS - 1)), Some(100), "rounded");
    assert_eq!(five.pace(Wall(RESETS)), Some(100));
    assert_eq!(five.pace(Wall(RESETS + HOUR)), Some(100), "after it resets");
    let week = window(WindowKind::Week, 0, Some(RESETS), RESETS);
    assert_eq!(
        week.pace(Wall(RESETS - 7 * 24 * HOUR + 24 * HOUR)),
        Some(14)
    );
    // Far from now either way, and nothing overflows.
    assert_eq!(week.pace(Wall(i64::MIN)), Some(0));
    assert_eq!(week.pace(Wall(i64::MAX)), Some(100));
    let far = window(WindowKind::Week, 0, Some(i64::MIN), RESETS);
    assert_eq!(far.pace(Wall(i64::MAX)), Some(100));
}

#[test]
fn pace_rounds_half_up() {
    // One percent of five hours is three minutes: half of one is ninety
    // seconds.
    let five = window(WindowKind::FiveHours, 0, Some(RESETS), RESETS);
    let start = RESETS - 5 * HOUR;
    assert_eq!(five.pace(Wall(start + 89_999)), Some(0));
    assert_eq!(five.pace(Wall(start + 90_000)), Some(1));
    assert_eq!(five.pace(Wall(RESETS - 90_001)), Some(99));
    assert_eq!(five.pace(Wall(RESETS - 90_000)), Some(100));
}

#[test]
fn a_window_with_no_reset_or_no_length_has_no_pace() {
    let none = window(WindowKind::Week, 10, None, RESETS);
    assert_eq!(none.pace(Wall(RESETS)), None);
    let other = window(WindowKind::Other("x".into()), 10, Some(RESETS), RESETS);
    assert_eq!(other.pace(Wall(RESETS - HOUR)), None);
}

#[test]
fn a_window_is_found_by_its_kind() {
    let kept = limits(vec![
        window(WindowKind::FiveHours, 1, None, 0),
        window(WindowKind::ModelWeek("opus".into()), 2, None, 0),
    ]);
    assert_eq!(kept.window(&WindowKind::FiveHours).map(|w| w.used), Some(1));
    let opus = WindowKind::ModelWeek("opus".into());
    assert_eq!(kept.window(&opus).map(|w| w.used), Some(2));
    assert_eq!(kept.window(&WindowKind::ModelWeek("sonnet".into())), None);
    assert_eq!(kept.window(&WindowKind::Week), None);
}

#[test]
fn news_replaces_only_the_windows_it_names_and_only_with_later_ones() {
    let mut kept = limits(vec![
        window(WindowKind::FiveHours, 10, Some(RESETS), 1000),
        window(WindowKind::Week, 40, Some(RESETS), 1000),
    ]);
    let news = limits(vec![window(WindowKind::FiveHours, 12, Some(RESETS), 2000)]);
    assert!(kept.merge(&news));
    assert_eq!(
        kept.window(&WindowKind::FiveHours).map(|w| w.used),
        Some(12)
    );
    assert_eq!(kept.window(&WindowKind::Week).map(|w| w.used), Some(40));
    // Older news changes nothing, however different.
    let old = limits(vec![
        window(WindowKind::FiveHours, 99, None, 1500),
        window(WindowKind::Week, 0, None, 999),
    ]);
    let before = kept.clone();
    assert!(!kept.merge(&old));
    assert_eq!(kept, before);
    // The same news again changes nothing either.
    assert!(!kept.merge(&news));
}

#[test]
fn windows_are_kept_in_their_kinds_order_then_others_by_name() {
    let mut kept = AccountLimits::default();
    let news = limits(vec![
        window(WindowKind::Other("zeta".into()), 1, None, 1),
        window(WindowKind::ExtraWeek, 1, None, 1),
        window(WindowKind::ModelWeek("sonnet".into()), 1, None, 1),
        window(WindowKind::Other("alpha".into()), 1, None, 1),
        window(WindowKind::Week, 1, None, 1),
        window(WindowKind::ModelWeek("opus".into()), 1, None, 1),
        window(WindowKind::FiveHours, 1, None, 1),
    ]);
    assert!(kept.merge(&news));
    let kinds: Vec<WindowKind> = kept.windows.iter().map(|w| w.kind.clone()).collect();
    assert_eq!(
        kinds,
        [
            WindowKind::FiveHours,
            WindowKind::Week,
            WindowKind::ModelWeek("opus".into()),
            WindowKind::ModelWeek("sonnet".into()),
            WindowKind::ExtraWeek,
            WindowKind::Other("alpha".into()),
            WindowKind::Other("zeta".into()),
        ]
    );
}

#[test]
fn two_heard_at_once_are_told_apart_the_cautious_way() {
    let at = 5000;
    let fine = |used, resets| window(WindowKind::Week, used, resets, at);
    let warned = |used| LimitWindow {
        state: LimitState::Warning,
        ..fine(used, Some(RESETS))
    };
    // The more used, then the worse state, then the later reset.
    let pairs = [
        (fine(30, Some(RESETS)), fine(31, Some(RESETS))),
        (fine(80, Some(RESETS)), warned(80)),
        (fine(80, Some(RESETS)), fine(80, Some(RESETS + 1))),
        (fine(80, None), fine(80, Some(RESETS))),
    ];
    for (lesser, greater) in pairs {
        for (first, second) in [(&lesser, &greater), (&greater, &lesser)] {
            let mut kept = limits(vec![first.clone()]);
            kept.merge(&limits(vec![second.clone()]));
            assert_eq!(
                kept.windows,
                std::slice::from_ref(&greater),
                "{first:?} and {second:?}"
            );
        }
    }
    // Extra usage in use wins over not.
    let extra = |in_use| AccountLimits {
        extra_usage: Some(ExtraUsage {
            in_use,
            heard: Wall(at),
        }),
        ..AccountLimits::default()
    };
    for (first, second) in [(extra(true), extra(false)), (extra(false), extra(true))] {
        let mut kept = first;
        kept.merge(&second);
        assert_eq!(kept.extra_usage.map(|extra| extra.in_use), Some(true));
    }
}

#[test]
fn a_refused_window_is_all_used_whoever_wrote_it() {
    let refused = LimitWindow {
        state: LimitState::Refused,
        ..window(WindowKind::FiveHours, 40, Some(RESETS), 1000)
    };
    let mut kept = AccountLimits::default();
    assert!(kept.merge(&limits(vec![refused])));
    assert_eq!(kept.windows[0].used, 100);
    // So it beats one more used heard at once, as the worse.
    let mut kept = limits(vec![window(WindowKind::FiveHours, 90, Some(RESETS), 1000)]);
    kept.merge(&limits(vec![LimitWindow {
        state: LimitState::Refused,
        ..window(WindowKind::FiveHours, 0, Some(RESETS), 1000)
    }]));
    assert_eq!(kept.windows[0].state, LimitState::Refused);
    assert_eq!(kept.windows[0].used, 100);
}

#[test]
fn extra_usage_is_replaced_as_a_window_is() {
    let extra = |in_use, heard| AccountLimits {
        extra_usage: Some(ExtraUsage {
            in_use,
            heard: Wall(heard),
        }),
        ..AccountLimits::default()
    };
    let mut kept = extra(true, 1000);
    assert!(kept.merge(&extra(false, 2000)));
    assert_eq!(kept.extra_usage.map(|extra| extra.in_use), Some(false));
    assert!(!kept.merge(&extra(true, 1500)), "older news");
    assert!(!kept.merge(&AccountLimits::default()), "no news");
    assert_eq!(kept.extra_usage.map(|extra| extra.in_use), Some(false));
}

#[test]
fn a_clearing_drops_everything_heard_before_it_and_is_kept() {
    let mut kept = limits(vec![
        window(WindowKind::FiveHours, 10, Some(RESETS), 1000),
        window(WindowKind::Week, 40, Some(RESETS), 3000),
    ]);
    kept.extra_usage = Some(ExtraUsage {
        in_use: true,
        heard: Wall(1000),
    });
    let clearing = AccountLimits {
        cleared: Some(Wall(2000)),
        ..AccountLimits::default()
    };
    assert!(kept.merge(&clearing));
    assert_eq!(
        kept.windows,
        [window(WindowKind::Week, 40, Some(RESETS), 3000)]
    );
    assert_eq!(kept.extra_usage, None);
    assert_eq!(kept.cleared, Some(Wall(2000)));
    // A saved copy from before never brings them back, and an older
    // clearing doesn't replace the later.
    let saved = AccountLimits {
        windows: vec![window(WindowKind::FiveHours, 10, Some(RESETS), 1000)],
        extra_usage: Some(ExtraUsage {
            in_use: true,
            heard: Wall(1000),
        }),
        cleared: Some(Wall(500)),
    };
    assert!(!kept.merge(&saved));
    assert_eq!(kept.cleared, Some(Wall(2000)));
    // News heard as it was cleared, or after, is kept.
    let at_once = limits(vec![window(WindowKind::FiveHours, 1, Some(RESETS), 2000)]);
    assert!(kept.merge(&at_once));
    assert_eq!(kept.windows.len(), 2);
}

#[test]
fn limits_read_unsorted_or_doubled_are_tidied_by_a_merge() {
    let mut kept = limits(vec![
        window(WindowKind::Week, 40, Some(RESETS), 1000),
        window(WindowKind::FiveHours, 10, Some(RESETS), 1000),
        window(WindowKind::FiveHours, 20, Some(RESETS), 2000),
    ]);
    assert!(kept.merge(&AccountLimits::default()));
    assert_eq!(
        kept.windows,
        [
            window(WindowKind::FiveHours, 20, Some(RESETS), 2000),
            window(WindowKind::Week, 40, Some(RESETS), 1000),
        ]
    );
}

/// Dice for limits made at random: xorshift, from a seed, so each is the
/// same every run.
struct Dice(u64);

impl Dice {
    /// A number below `sides`.
    fn roll(&mut self, sides: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % sides
    }

    /// A small number below `sides`, as an `i64`.
    fn small(&mut self, sides: u64) -> i64 {
        i64::try_from(self.roll(sides)).expect("small")
    }

    /// Limits made at random, from few enough choices that two of a kind
    /// are often heard at once, and clearings often fall between them.
    fn limits(&mut self) -> AccountLimits {
        let kinds = every_kind();
        let states = [LimitState::Fine, LimitState::Warning, LimitState::Refused];
        let windows = (0..self.roll(4))
            .map(|_| LimitWindow {
                kind: kinds[usize::try_from(self.roll(5)).expect("small")].clone(),
                used: u8::try_from(self.roll(3) * 40).expect("small"),
                resets: (self.roll(3) > 0).then(|| Wall(RESETS + self.small(2))),
                state: states[usize::try_from(self.roll(3)).expect("small")],
                heard: Wall(self.small(4)),
            })
            .collect();
        let extra_usage = (self.roll(2) == 0).then(|| ExtraUsage {
            in_use: self.roll(2) == 0,
            heard: Wall(self.small(4)),
        });
        let cleared = (self.roll(3) == 0).then(|| Wall(self.small(4)));
        AccountLimits {
            windows,
            extra_usage,
            cleared,
        }
    }
}

/// Every order of `items`.
fn orders<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut all = Vec::new();
    for (at, first) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(at);
        for mut order in orders(&rest) {
            order.insert(0, first.clone());
            all.push(order);
        }
    }
    all
}

#[test]
fn merging_ends_the_same_in_any_order() {
    for seed in 1..=300_u64 {
        let mut dice = Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let news: Vec<AccountLimits> = (0..5).map(|_| dice.limits()).collect();
        let mut ends = orders(&news).into_iter().map(|order| {
            let mut kept = AccountLimits::default();
            for each in &order {
                kept.merge(each);
            }
            kept
        });
        let first = ends.next().expect("an order");
        for end in ends {
            assert_eq!(end, first, "seed {seed}: {news:#?}");
        }
        // Whichever side each is on.
        for one in &news {
            for other in &news {
                let (mut a, mut b) = (one.clone(), other.clone());
                a.merge(other);
                b.merge(one);
                assert_eq!(a, b, "seed {seed}");
            }
        }
        // A refused window is all used, and none is from before the
        // clearing.
        for window in &first.windows {
            assert!(window.state != LimitState::Refused || window.used == 100);
            assert!(first.cleared.is_none_or(|cleared| window.heard >= cleared));
        }
    }
}

#[test]
fn merge_says_whether_anything_changed() {
    for seed in 1..=300_u64 {
        let mut dice = Dice(seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
        let mut kept = AccountLimits::default();
        for _ in 0..6 {
            let news = dice.limits();
            let before = kept.clone();
            let changed = kept.merge(&news);
            assert_eq!(changed, kept != before, "seed {seed}");
            assert!(!kept.clone().merge(&news), "taking it in again");
        }
    }
}

#[test]
fn windows_are_forgotten_once_their_reset_has_passed() {
    let mut kept = limits(vec![
        window(WindowKind::FiveHours, 10, Some(RESETS), 0),
        window(WindowKind::Week, 40, Some(RESETS + HOUR), 0),
        window(WindowKind::Other("x".into()), 1, None, 0),
    ]);
    assert!(!kept.forget_passed(Wall(RESETS - 1)));
    assert_eq!(kept.windows.len(), 3);
    assert!(kept.forget_passed(Wall(RESETS)), "at its reset");
    assert_eq!(kept.windows.len(), 2);
    assert!(kept.forget_passed(Wall(RESETS + 2 * HOUR)));
    // One with no reset stays.
    assert_eq!(
        kept.windows,
        [window(WindowKind::Other("x".into()), 1, None, 0)]
    );
    assert!(!kept.forget_passed(Wall(i64::MAX)));
}

#[test]
fn the_next_reset_is_the_earliest() {
    assert_eq!(AccountLimits::default().next_reset(), None);
    let kept = limits(vec![
        window(WindowKind::FiveHours, 10, None, 0),
        window(WindowKind::Week, 40, Some(RESETS + HOUR), 0),
        window(WindowKind::ExtraWeek, 40, Some(RESETS), 0),
    ]);
    assert_eq!(kept.next_reset(), Some(Wall(RESETS)));
    assert_eq!(
        limits(vec![window(WindowKind::Week, 1, None, 0)]).next_reset(),
        None
    );
}

#[test]
fn a_profile_is_found_by_its_slug() {
    for profile in [
        Builtin::ClaudeAgentAcp,
        Builtin::TestNpm,
        Builtin::TestDownload,
        Builtin::TestPlain,
        Builtin::TestModels,
        #[cfg(feature = "fake")]
        Builtin::Fake,
    ] {
        assert_eq!(Builtin::from_slug(profile.slug()), Some(profile));
    }
    assert_eq!(Builtin::from_slug("claude"), None);
    assert_eq!(Builtin::from_slug(""), None);
}

/// `fixtures/pace.json`: the pace and length of a window of every kind at
/// many instants, as caf-core works them out: what the SDKs check their
/// own against. Before the window starts, at its start, through it, at
/// the edges of rounding, at its reset and after; and with no reset.
fn paces() -> String {
    let mut entries = Vec::new();
    for kind in every_kind() {
        let length = kind
            .length()
            .map(|length| i64::try_from(length.as_millis()).expect("a window's length fits"));
        let span = length.unwrap_or(5 * HOUR);
        let start = RESETS - span;
        // A hundredth of the window, and half of one.
        let (hundredth, half) = (span / 100, span / 200);
        let mut instants = vec![
            start - HOUR,
            start - 1,
            start,
            start + 1,
            start + half - 1,
            start + half,
            start + half + 1,
            start + hundredth,
            start + span / 4,
            start + span / 2,
            start + 37 * hundredth + half,
            RESETS - half - 1,
            RESETS - half,
            RESETS - 1,
            RESETS,
            RESETS + 1,
            RESETS + HOUR,
        ];
        instants.sort_unstable();
        for (resets, at) in instants
            .iter()
            .map(|&at| (Some(RESETS), at))
            .chain([(None, start + span / 2)])
        {
            let window = LimitWindow {
                kind: kind.clone(),
                used: 37,
                resets: resets.map(Wall),
                state: LimitState::Fine,
                heard: Wall(start),
            };
            let entry = json!({
                "window": window,
                "now": at,
                "pace": window.pace(Wall(at)),
                "length": length,
            });
            entries.push(format!("  {entry}"));
        }
    }
    format!("[\n{}\n]\n", entries.join(",\n"))
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test that writes and reads the fixture: the core itself still does no I/O"
)]
fn the_pace_fixture_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pace.json");
    let text = paces();
    let old = std::fs::read_to_string(&path).ok();
    if old.as_deref() == Some(text.as_str()) {
        return;
    }
    let bless = "CAF_BLESS=1 cargo test -p caf-core the_pace_fixture";
    assert!(
        std::env::var_os("CAF_BLESS").is_some(),
        "fixtures/pace.json is {}: run `{bless}`",
        if old.is_some() { "stale" } else { "missing" }
    );
    let new = path.with_extension("json.new");
    std::fs::write(&new, &text).expect("it's written");
    std::fs::rename(&new, &path).expect("it's moved into place");
}

#[test]
fn the_pace_fixture_holds_every_kind_and_the_edges() {
    let entries: Vec<Value> = serde_json::from_str(&paces()).expect("it's JSON");
    for kind in every_kind() {
        let kind = serde_json::to_value(&kind).expect("a kind serialises");
        let of_kind: Vec<&Value> = (entries.iter())
            .filter(|entry| entry["window"]["kind"] == kind)
            .collect();
        assert!(
            of_kind
                .iter()
                .any(|entry| entry["window"]["resets"].is_null())
        );
        if kind["type"] == "Other" {
            assert!(of_kind.iter().all(|entry| entry["pace"].is_null()));
            assert!(of_kind.iter().all(|entry| entry["length"].is_null()));
            continue;
        }
        let paces: Vec<&Value> = of_kind.iter().map(|entry| &entry["pace"]).collect();
        for pace in [0, 1, 25, 38, 50, 99, 100] {
            assert!(paces.contains(&&json!(pace)), "{kind}: {pace}");
        }
    }
}

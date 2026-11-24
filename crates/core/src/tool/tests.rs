//! Tests of the tool-call model's rules and caps.

use serde_json::{Value, json};

use super::{
    Action, CafTool, Cut, Diff, DiffLine, Hunk, LineChange, Location, MOST_DIFF, MOST_IMAGE,
    MOST_INPUT, MOST_LOCATIONS, MOST_TEXT, Text, ToolKind, ToolStatus, capped_input,
    capped_locations, image,
};

#[test]
fn a_status_has_ended_once_it_cant_change() {
    use ToolStatus::*;
    for status in [Done, Failed, Refused, Cancelled] {
        assert!(status.ended(), "{status:?}");
    }
    for status in [Pending, Asking, Running] {
        assert!(!status.ended(), "{status:?}");
    }
}

#[test]
fn cafs_tools_are_named_as_served() {
    let names: Vec<_> = CafTool::ALL.into_iter().map(CafTool::name).collect();
    assert_eq!(names, ["Read", "Edit", "Write", "Bash"]);
    for tool in CafTool::ALL {
        assert_eq!(CafTool::named(tool.name()), Some(tool));
    }
    assert_eq!(CafTool::named("read"), None);
    assert_eq!(CafTool::named("WebSearch"), None);
}

#[test]
fn an_action_says_its_kind() {
    let path = || "/a".to_owned();
    let cases = [
        (
            Action::Read {
                path: path(),
                lines: None,
            },
            ToolKind::Read,
        ),
        (Action::Edit { path: path() }, ToolKind::Edit),
        (Action::Write { path: path() }, ToolKind::Edit),
        (
            Action::Run {
                command: "ls".into(),
                folder: path(),
                background: false,
            },
            ToolKind::Execute,
        ),
        (Action::Tool { name: "x".into() }, ToolKind::Other),
    ];
    for (action, kind) in cases {
        assert_eq!(action.kind(), kind, "{action:?}");
    }
}

#[test]
fn text_keeps_its_start_at_a_character_boundary() {
    assert_eq!(
        Text::new("short"),
        Text {
            text: "short".into(),
            cut: None
        }
    );
    let exact = "a".repeat(MOST_TEXT);
    assert_eq!(Text::new(exact.clone()).cut, None);
    // A two-byte character straddles the cap: it goes whole.
    let long = format!("{}é{}", "a".repeat(MOST_TEXT - 1), "b".repeat(10));
    let text = Text::new(long.clone());
    assert_eq!(text.text, "a".repeat(MOST_TEXT - 1));
    assert_eq!(
        text.cut,
        Some(Cut {
            kept: (MOST_TEXT - 1) as u64,
            total: long.len() as u64
        })
    );
}

#[test]
fn input_too_long_becomes_the_start_of_its_json() {
    let small = json!({ "path": "/a" });
    assert_eq!(capped_input(small.clone()), (small, None));
    let big = json!({ "content": "é".repeat(MOST_INPUT) });
    let json = big.to_string();
    let (input, cut) = capped_input(big);
    let Value::String(start) = input else {
        panic!("not a string: {input:?}");
    };
    assert!(json.starts_with(&start));
    assert!(start.len() <= MOST_INPUT && start.len() > MOST_INPUT - 2);
    assert_eq!(
        cut,
        Some(Cut {
            kept: start.len() as u64,
            total: json.len() as u64
        })
    );
}

fn line(change: LineChange, old: Option<u64>, new: Option<u64>, text: &str) -> DiffLine {
    DiffLine {
        change,
        old,
        new,
        text: text.into(),
    }
}

#[test]
fn a_diff_is_hunks_with_three_lines_around_each_change() {
    let before: String = (1..=20).map(|n| format!("line {n}\n")).collect();
    let after = before
        .replace("line 2\n", "line two\n")
        .replace("line 15\n", "")
        .replace("line 20\n", "line 20\nline 21\n");
    let diff = Diff::new("/f", Some(&before), &after);
    use LineChange::*;
    assert_eq!(diff.path, "/f");
    assert!(!diff.new_file);
    assert_eq!((diff.added, diff.removed, diff.more), (2, 2, 0));
    // Line 2's change, then 15's and 20's, whose context meets.
    assert_eq!(diff.hunks.len(), 2);
    assert_eq!(
        diff.hunks[0],
        Hunk {
            lines: vec![
                line(Kept, Some(1), Some(1), "line 1"),
                line(Removed, Some(2), None, "line 2"),
                line(Added, None, Some(2), "line two"),
                line(Kept, Some(3), Some(3), "line 3"),
                line(Kept, Some(4), Some(4), "line 4"),
                line(Kept, Some(5), Some(5), "line 5"),
            ]
        }
    );
    let second = &diff.hunks[1].lines;
    assert_eq!(second[0], line(Kept, Some(12), Some(12), "line 12"));
    assert_eq!(second[3], line(Removed, Some(15), None, "line 15"));
    assert_eq!(second[4], line(Kept, Some(16), Some(15), "line 16"));
    assert_eq!(second.last(), Some(&line(Added, None, Some(20), "line 21")));
}

#[test]
fn a_new_file_is_all_added() {
    let diff = Diff::new("/new", None, "a\r\nb");
    assert!(diff.new_file);
    assert_eq!((diff.added, diff.removed), (2, 0));
    assert_eq!(
        diff.hunks,
        [Hunk {
            lines: vec![
                line(LineChange::Added, None, Some(1), "a"),
                line(LineChange::Added, None, Some(2), "b"),
            ]
        }]
    );
    // An empty file there before isn't a new one.
    assert!(!Diff::new("/f", Some(""), "a").new_file);
}

#[test]
fn nothing_hides_in_a_diff() {
    let diff = Diff::new("/f", Some("a\n"), "a\u{202e}b\x1b[31m\n");
    let added = &diff.hunks[0].lines[1];
    assert_eq!(added.text, "a<U+202E>b^[[31m");
}

#[test]
fn a_diff_keeps_whole_hunks_up_to_its_cap_and_counts_the_rest() {
    // Ten hunks of one big changed line each, far apart.
    let big = "x".repeat(MOST_DIFF / 4);
    let before: String = (0..100).map(|n| format!("{n}\n")).collect();
    let after: String = (0..100)
        .map(|n| {
            if n % 10 == 5 {
                format!("{big}\n")
            } else {
                format!("{n}\n")
            }
        })
        .collect();
    let diff = Diff::new("/f", Some(&before), &after);
    assert_eq!((diff.added, diff.removed), (10, 10));
    // Each hunk is a quarter of the cap and a little: three fit.
    assert_eq!(diff.hunks.len(), 3);
    assert_eq!(diff.more, 7);
    let size: usize = (diff.hunks.iter())
        .flat_map(|hunk| &hunk.lines)
        .map(|line| line.text.len())
        .sum();
    assert!(size <= MOST_DIFF);
}

#[test]
fn an_image_too_big_keeps_only_its_size() {
    let small = image("image/png".into(), "aGVsbG8=".into());
    assert_eq!((small.size, small.data.as_deref()), (5, Some("aGVsbG8=")));
    let fits = image("image/png".into(), "A".repeat(MOST_IMAGE / 3 * 4));
    assert!(fits.data.is_some());
    let big = image("image/png".into(), "A".repeat((MOST_IMAGE + 3) / 3 * 4));
    assert_eq!(big.data, None);
    assert!(big.size > MOST_IMAGE as u64);
}

#[test]
fn locations_keep_the_first_and_count_the_rest() {
    let at = |n: u64| Location {
        path: format!("/{n}"),
        line: Some(n),
    };
    let (kept, more) = capped_locations((0..3).map(at).collect());
    assert_eq!((kept.len(), more), (3, 0));
    let (kept, more) = capped_locations((0..MOST_LOCATIONS as u64 + 5).map(at).collect());
    assert_eq!((kept.len(), more), (MOST_LOCATIONS, 5));
    assert_eq!(kept[0], at(0));
}

#[test]
fn a_diff_too_big_to_work_out_is_counted_not_worked_out() {
    // Two big files with nothing in common: working out their hunks would
    // take seconds, and block every chat.
    let before: String = (0..30_000).map(|n| format!("old {n}\n")).collect();
    let after: String = (0..30_000).map(|n| format!("new {n}\n")).collect();
    let diff = Diff::new("/f", Some(&before), &after);
    assert_eq!(
        (diff.hunks.len(), diff.added, diff.removed, diff.more),
        (0, 30_000, 30_000, 1)
    );
    // Past 10,000 lines changed, however small they are, it's not done.
    let short: String = (0..5_001).map(|n| format!("{n}\n")).collect();
    let others: String = (0..5_000).map(|n| format!("x{n}\n")).collect();
    let diff = Diff::new("/f", Some(&short), &others);
    assert_eq!(
        (diff.hunks.len(), diff.added, diff.removed, diff.more),
        (0, 5_000, 5_001, 1)
    );
    // A big file with a small change is worked out: what's the same at
    // either end costs nothing.
    let changed = before.replacen("old 15000\n", "new 15000\n", 1);
    let diff = Diff::new("/f", Some(&before), &changed);
    assert_eq!(
        (diff.hunks.len(), diff.added, diff.removed, diff.more),
        (1, 1, 1, 0)
    );
    assert_eq!(diff.hunks[0].lines[3].old, Some(15_001));
}

#[test]
fn short_lines_count_against_a_diffs_cap_too() {
    // 5,000 empty lines are no text, but far from nothing to keep.
    let diff = Diff::new("/f", None, &"\n".repeat(5_000));
    assert_eq!((diff.hunks.len(), diff.added, diff.more), (0, 5_000, 1));
}

#[test]
fn a_diffs_lines_are_counted_as_they_are_compared_a_lone_return_ending_one() {
    // Old Mac line endings: no `\n` at all, but 6,000 lines, every tenth
    // changed: 11,982 lines between the first change and the last.
    let before: String = (0..6_000).map(|n| format!("{n}\r")).collect();
    let after: String = (0..6_000)
        .map(|n| {
            if n % 10 == 0 {
                format!("x{n}\r")
            } else {
                format!("{n}\r")
            }
        })
        .collect();
    let diff = Diff::new("/f", Some(&before), &after);
    assert_eq!(
        (diff.hunks.len(), diff.added, diff.removed, diff.more),
        (0, 5_991, 5_991, 1)
    );
    // Lines that end in either way are each a line.
    let diff = Diff::new("/f", Some("a\rb\r\nc\n"), "a\rB\r\nc\n");
    assert_eq!((diff.added, diff.removed), (1, 1));
    assert_eq!(diff.hunks[0].lines[1].text, "b");
}

#[test]
fn an_image_is_made_from_its_bytes_or_base64_and_capped() {
    let cases: [(&[u8], &str); 5] = [
        (b"", ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (&[0xfb, 0xff, 0x00, 0x7f], "+/8Afw=="),
    ];
    for (bytes, text) in cases {
        let image = super::Image::new("image/png", bytes);
        assert_eq!(image.data.as_deref(), Some(text), "{bytes:?}");
        assert_eq!(image.size, bytes.len() as u64);
        assert_eq!(image.mime, "image/png");
        assert_eq!(super::Image::from_base64("image/png", text), image);
    }
    let biggest = vec![1u8; super::MOST_IMAGE];
    assert!(super::Image::new("image/png", &biggest).data.is_some());
    let bigger = vec![1u8; super::MOST_IMAGE + 1];
    let image = super::Image::new("image/png", &bigger);
    assert_eq!(image.data, None, "only its size is kept");
    assert_eq!(image.size, bigger.len() as u64);
    let text = super::base64(&bigger);
    assert_eq!(super::Image::from_base64("image/png", text), image);
}

#[test]
fn a_location_is_a_path_and_perhaps_a_line() {
    let location = super::Location::new("/work/a.rs", Some(3));
    assert_eq!(
        serde_json::to_value(&location).expect("JSON"),
        json!({ "path": "/work/a.rs", "line": 3 })
    );
    assert_eq!(super::Location::new("/work/a.rs", None).line, None);
}

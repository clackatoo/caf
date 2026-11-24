use proptest::prelude::*;

use super::*;

/// The lines `bytes` come to, fed at once.
fn lines(bytes: &[u8]) -> Vec<String> {
    let mut all = Vec::new();
    let mut screen = Screen::new();
    if let Some(redraw) = screen.feed(bytes) {
        apply(&mut all, redraw);
    }
    all
}

/// `redraw` applied to `all`, as the core applies it.
fn apply(all: &mut Vec<String>, redraw: Redraw) {
    let from = usize::try_from(redraw.from).unwrap();
    assert!(from <= all.len(), "a gap: from {from} of {}", all.len());
    all.truncate(from);
    all.extend(redraw.lines);
}

/// The lines `chunks` come to, fed one after another.
fn fed(chunks: &[&[u8]]) -> Vec<String> {
    let mut all = Vec::new();
    let mut screen = Screen::new();
    for chunk in chunks {
        if let Some(redraw) = screen.feed(chunk) {
            apply(&mut all, redraw);
        }
    }
    all
}

#[test]
fn plain_lines_are_lines() {
    assert_eq!(lines(b"one\ntwo\n"), ["one", "two"]);
    assert_eq!(lines(b"one\r\ntwo"), ["one", "two"]);
    assert_eq!(lines(b""), Vec::<String>::new());
}

#[test]
fn blank_lines_between_are_kept_and_trailing_ones_arent() {
    assert_eq!(lines(b"a\n\n\nb\n\n\n"), ["a", "", "", "b"]);
    // Blank lines first are output too, until they scroll away.
    assert_eq!(lines(b"\n\nx"), ["", "", "x"]);
}

#[test]
fn a_progress_bar_is_its_last_drawing() {
    assert_eq!(
        lines(b"Downloading  10%\rDownloading  50%\rDownloading 100%\ndone\n"),
        ["Downloading 100%", "done"]
    );
    // Drawn over several feeds, each change replacing the line.
    let mut screen = Screen::new();
    assert_eq!(
        screen.feed(b"get 1%"),
        Some(Redraw {
            from: 0,
            lines: vec!["get 1%".into()]
        })
    );
    assert_eq!(
        screen.feed(b"\rget 2%"),
        Some(Redraw {
            from: 0,
            lines: vec!["get 2%".into()]
        })
    );
    assert_eq!(screen.feed(b"\x1b[?25l"), None, "nothing changed");
}

#[test]
fn a_shorter_redraw_leaves_the_rest_unless_erased() {
    assert_eq!(lines(b"hello\rhi"), ["hillo"]);
    assert_eq!(lines(b"hello\rhi\x1b[K"), ["hi"]);
    assert_eq!(lines(b"hello\r\x1b[2Khi"), ["hi"]);
    assert_eq!(lines(b"hello\x1b[3D\x1b[1K"), ["   lo"]);
}

#[test]
fn backspace_moves_back() {
    assert_eq!(lines(b"abc\x08\x08X"), ["aXc"]);
    assert_eq!(lines(b"\x08\x08a"), ["a"]);
}

#[test]
fn tabs_move_to_the_next_stop() {
    assert_eq!(lines(b"a\tb"), ["a       b"]);
    assert_eq!(lines(b"12345678\tb"), ["12345678        b"]);
}

#[test]
fn the_cursor_moves_up_to_redraw_lines() {
    // Docker-style: several bars, each redrawn in place.
    let out = lines(b"a: 0%\nb: 0%\n\x1b[2A\ra: 100%\x1b[K\n\rb: 50%\x1b[K\n");
    assert_eq!(out, ["a: 100%", "b: 50%"]);
}

#[test]
fn moving_up_stops_at_the_windows_top() {
    let mut input = Vec::new();
    for n in 0..30 {
        input.extend_from_slice(format!("line {n}\n").as_bytes());
    }
    input.extend_from_slice(b"\x1b[100AX");
    let out = lines(&input);
    assert_eq!(out.len(), 30);
    // The window's top is line 30 - 23 = 7: its first character changed.
    assert_eq!(out[7], "Xine 7");
    assert_eq!(out[6], "line 6");
}

#[test]
fn lines_scrolled_away_stay_as_they_were() {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    for n in 0..40 {
        chunks.push(format!("line {n}\n").into_bytes());
    }
    chunks.push(b"\x1b[2J".to_vec());
    let chunks: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
    let out = fed(&chunks);
    // Clearing the screen clears the window: the 17 lines above it stay.
    assert_eq!(out.len(), 17);
    assert_eq!(out[16], "line 16");
}

#[test]
fn clearing_the_screen_and_drawing_again() {
    let out = fed(&[b"old 1\nold 2\n", b"\x1b[H\x1b[2Jnew\n"]);
    assert_eq!(out, ["new"]);
}

#[test]
fn erasing_below_the_cursor() {
    assert_eq!(lines(b"a\nb\nc\x1b[2;1H\x1b[J"), ["a"]);
    // Erased characters on the line are blanks, as the terminal shows them.
    assert_eq!(lines(b"a\nb\nc\x1b[2;2H\x1b[1J"), ["", " ", "c"]);
}

#[test]
fn moving_to_a_column_and_a_place() {
    assert_eq!(lines(b"abcdef\x1b[3GX"), ["abXdef"]);
    assert_eq!(lines(b"abc\x1b[10GX"), ["abc      X"]);
    assert_eq!(lines(b"x\x1b[3;4HY"), ["x", "", "   Y"]);
    assert_eq!(lines(b"x\x1b[3dY"), ["x", "", " Y"]);
    assert_eq!(lines(b"a\nb\x1b[FZ"), ["Z", "b"]);
    assert_eq!(lines(b"a\x1b[EZ"), ["a", "Z"]);
    assert_eq!(lines(b"ab\x1b[1CZ"), ["ab Z"]);
    assert_eq!(lines(b"x\x1b[50BY"), {
        let mut want = vec![String::from("x")];
        want.extend(std::iter::repeat_n(String::new(), ROWS - 2));
        want.push(" Y".into());
        want
    });
}

#[test]
fn saving_and_restoring_the_cursor() {
    assert_eq!(lines(b"a\x1b7bc\x1b8X"), ["aXc"]);
    assert_eq!(lines(b"a\x1b[sbc\x1b[uX"), ["aXc"]);
}

#[test]
fn erasing_deleting_and_inserting_characters() {
    assert_eq!(lines(b"abcdef\x1b[3G\x1b[2X"), ["ab  ef"]);
    assert_eq!(lines(b"abcdef\x1b[3G\x1b[2P"), ["abef"]);
    assert_eq!(lines(b"abcdef\x1b[3G\x1b[2@"), ["ab  cdef"]);
}

#[test]
fn inserting_and_deleting_lines() {
    assert_eq!(lines(b"a\nb\nc\x1b[2;1H\x1b[M"), ["a", "c"]);
    assert_eq!(lines(b"a\nb\x1b[1;1H\x1b[L"), ["", "a", "b"]);
}

#[test]
fn colours_survive_each_line_on_its_own() {
    assert_eq!(
        lines(b"\x1b[31mred\x1b[0m plain"),
        ["\x1b[0;31mred\x1b[0m plain"]
    );
    // A colour across lines: each starts it again and resets at its end.
    assert_eq!(
        lines(b"\x1b[1;32mgreen\nstill\x1b[m\n"),
        ["\x1b[0;1;32mgreen\x1b[0m", "\x1b[0;1;32mstill\x1b[0m"]
    );
}

#[test]
fn every_kind_of_colour_is_kept() {
    assert_eq!(lines(b"\x1b[91;44mx"), ["\x1b[0;91;44mx\x1b[0m"]);
    assert_eq!(lines(b"\x1b[38;5;208mx"), ["\x1b[0;38;5;208mx\x1b[0m"]);
    assert_eq!(lines(b"\x1b[48;2;1;2;3mx"), ["\x1b[0;48;2;1;2;3mx\x1b[0m"]);
    assert_eq!(lines(b"\x1b[38:2::1:2:3mx"), ["\x1b[0;38;2;1;2;3mx\x1b[0m"]);
    assert_eq!(lines(b"\x1b[38:5:9mx"), ["\x1b[0;91mx\x1b[0m"]);
    assert_eq!(
        lines(b"\x1b[1;2;3;4;5;7;8;9mx"),
        ["\x1b[0;1;2;3;4;5;7;8;9mx\x1b[0m"]
    );
}

#[test]
fn attributes_turn_off() {
    assert_eq!(
        lines(b"\x1b[1;4mA\x1b[22mB\x1b[24mC"),
        ["\x1b[0;1;4mA\x1b[0;4mB\x1b[0mC"]
    );
    assert_eq!(
        lines(b"\x1b[31;42mA\x1b[39mB\x1b[49mC"),
        ["\x1b[0;31;42mA\x1b[0;42mB\x1b[0mC"]
    );
}

#[test]
fn bad_colours_are_ignored() {
    assert_eq!(lines(b"\x1b[38;7mx"), ["x"]);
    assert_eq!(lines(b"\x1b[38mx"), ["x"]);
    assert_eq!(lines(b"\x1b[38;5;999mx"), ["\x1b[0;38;5;255mx\x1b[0m"]);
}

#[test]
fn other_sequences_are_dropped() {
    // A title, a hyperlink, DCS, private modes, bracketed paste, a bell.
    let out = lines(
        b"\x1b]0;title\x07\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ \x1bP1$r\x1b\\\x1b[?1049h\x1b[?2004hok\x07",
    );
    assert_eq!(out, ["link ok"]);
}

#[test]
fn utf8_is_read_across_feeds() {
    let crab = "🦀".as_bytes();
    let out = fed(&[&crab[..1], &crab[1..3], &crab[3..], b"\n"]);
    assert_eq!(out, ["🦀"]);
}

#[test]
fn a_character_split_across_feeds_keeps_the_one_after() {
    // What vte 0.15.0 gets wrong on its own: "a" was lost.
    assert_eq!(fed(&[b"\xc3", b"\xa9a\xf0\x9f\xa6\x80"]), ["éa🦀"]);
    assert_eq!(fed(&[b"x\xc3", b"\xa9a\xf0\x9f", b"\xa6\x80b"]), ["xéa🦀b"]);
}

#[test]
fn bad_utf8_is_a_replacement_character() {
    let out = lines(b"a\xFFb");
    assert_eq!(out, ["a\u{FFFD}b"]);
}

#[test]
fn a_reset_clears_the_window() {
    assert_eq!(lines(b"\x1b[31mabc\nd\x1bcx"), ["x"]);
}

#[test]
fn a_long_line_isnt_wrapped_short_of_16_kib() {
    let long = "x".repeat(5000);
    assert_eq!(lines(long.as_bytes()), [long]);
    let longer = "y".repeat(MOST_LINE + 10);
    let out = lines(longer.as_bytes());
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].len(), MOST_LINE);
    assert_eq!(out[1].len(), 10);
}

#[test]
fn a_long_line_is_broken_by_its_bytes() {
    // Two bytes a character: half as many to a line.
    let out = lines("é".repeat(MOST_LINE).as_bytes());
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|line| line.len() == MOST_LINE));
}

#[test]
fn a_long_coloured_line_is_broken_into_lines_each_coloured_on_its_own() {
    // Every character a colour of its own, as a rainbow prints it.
    let mut input = String::new();
    for n in 0..MOST_LINE {
        input.push_str(&format!("\x1b[38;5;{}mx", n % 256));
    }
    let out = lines(input.as_bytes());
    assert!(out.len() > 1);
    for line in &out {
        assert!(line.len() <= MOST_LINE, "{} bytes", line.len());
        assert!(line.starts_with("\x1b[0;"));
        assert!(line.ends_with("\x1b[0m"));
    }
    let text: String = out.iter().map(|line| strip_sgr(line)).collect();
    assert_eq!(text, "x".repeat(MOST_LINE));
}

/// A line of red `x`s whose rendered bytes are exactly [`MOST_LINE`]:
/// its code, the `x`s, and the reset.
fn full_red_line() -> Vec<u8> {
    let count = MOST_LINE - "\x1b[0;31m".len() - "\x1b[0m".len();
    format!("\x1b[31m{}\x1b[0m", "x".repeat(count)).into_bytes()
}

#[test]
fn erasing_characters_in_a_full_line_keeps_it_within_the_cap() {
    // A blank in a red line costs a reset and the red again.
    let mut input = full_red_line();
    input.extend_from_slice(b"\x1b[100G\x1b[3X");
    let out = lines(&input);
    assert_eq!(out.len(), 1);
    assert!(out[0].len() <= MOST_LINE, "{} bytes", out[0].len());
    assert!(strip_sgr(&out[0]).starts_with(&format!("{}   x", "x".repeat(99))));
}

#[test]
fn rewriting_a_character_of_a_full_line_drops_only_from_its_end() {
    // A blue x among red ones costs two codes more: the line's end goes,
    // not everything after the cursor.
    let mut input = full_red_line();
    input.extend_from_slice(b"\x1b[100G\x1b[34mB\x1b[0m");
    let out = lines(&input);
    assert_eq!(out.len(), 1, "nothing goes on to the next line");
    assert!(out[0].len() <= MOST_LINE);
    let text = strip_sgr(&out[0]);
    assert!(text.starts_with(&format!("{}B{}", "x".repeat(99), "x".repeat(1000))));
}

#[test]
fn a_character_past_a_nearly_full_lines_end_leaves_the_line_as_it_was() {
    // Moving the cursor past the end writes nothing: the red Z doesn't fit,
    // so it goes on the next line, and the line isn't padded to it.
    let plain = "x".repeat(MOST_LINE - 5);
    let chunks: [&[u8]; 2] = [plain.as_bytes(), b"\x1b[16384G\x1b[31mZ\x1b[0m"];
    let whole = chunks.concat();
    assert_eq!(fed(&chunks), lines(&whole));
    assert_eq!(lines(&whole)[0], plain);
    assert_eq!(lines(&whole)[1], "\x1b[0;31mZ\x1b[0m");
}

#[test]
fn reverse_index_at_the_top_scrolls_down() {
    assert_eq!(lines(b"top\x1b[H\x1bMnew\n"), ["new", "top"]);
    // The bottom line goes.
    let mut input = Vec::new();
    for n in 0..ROWS {
        input.extend_from_slice(format!("{n}\r\n").as_bytes());
    }
    input.extend_from_slice(b"\x1b[H\x1bMx");
    let out = lines(&input);
    assert_eq!(out[1], "x");
    assert_eq!(out[2], "1");
    assert_eq!(out.len(), ROWS + 1);
}

#[test]
fn finishing_inside_an_escape_shows_nothing() {
    // The part of a character is the title's, which is dropped.
    let mut screen = Screen::new();
    let mut all = Vec::new();
    apply(&mut all, screen.feed(b"ok\x1b]0;title\xE2\x82").unwrap());
    if let Some(redraw) = screen.finish() {
        apply(&mut all, redraw);
    }
    assert_eq!(all, ["ok"]);
}

#[test]
fn finishing_gives_a_last_character_cut_short() {
    let mut screen = Screen::new();
    let mut all = Vec::new();
    apply(&mut all, screen.feed(b"ok\xE2\x82").unwrap());
    assert_eq!(all, ["ok"]);
    apply(&mut all, screen.finish().unwrap());
    assert_eq!(all, ["ok\u{FFFD}"]);
    assert_eq!(screen.finish(), None);
    // Nothing cut short, nothing to give.
    let mut screen = Screen::new();
    screen.feed(b"ok");
    assert_eq!(screen.finish(), None);
}

#[test]
fn changes_from_feeds_give_the_same_lines_as_one() {
    let input: &[u8] = b"\x1b[32mok\x1b[0m 1\nwork 10%\rwork 90%\rwork done\nx\x1b[1A\x1b[2Ky\n\nz";
    let at_once = lines(input);
    for size in 1..input.len() {
        let chunks: Vec<&[u8]> = input.chunks(size).collect();
        assert_eq!(fed(&chunks), at_once, "in chunks of {size}");
    }
}

#[test]
fn a_change_gives_only_the_lines_from_the_first_changed() {
    let mut screen = Screen::new();
    screen.feed(b"a\nb\nc");
    assert_eq!(
        screen.feed(b"d"),
        Some(Redraw {
            from: 2,
            lines: vec!["cd".into()]
        })
    );
    assert_eq!(
        screen.feed(b"\ne\n"),
        Some(Redraw {
            from: 3,
            lines: vec!["e".into()]
        })
    );
    assert_eq!(screen.feed(b"\n"), None);
}

#[test]
fn many_lines_in_one_feed_are_all_given() {
    let input = "x\n".repeat(10_000);
    let out = lines(input.as_bytes());
    assert_eq!(out.len(), 10_000);
}

#[test]
fn strip_sgr_leaves_the_text() {
    assert_eq!(strip_sgr("\x1b[0;31mred\x1b[0m plain"), "red plain");
    assert_eq!(strip_sgr("\x1b[0;38;2;1;2;3mé\x1b[0m"), "é");
    assert_eq!(strip_sgr("no codes"), "no codes");
    assert_eq!(strip_sgr("\x1b[1"), "", "cut short");
    assert_eq!(strip_sgr("a\x1bb"), "a", "ESC b is a sequence of its own");
}

#[test]
fn strip_sgr_drops_every_kind_of_escape() {
    // OSC 8 links, ended by ST or BEL, as an app's own terminal might keep.
    assert_eq!(
        strip_sgr("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ done"),
        "link done"
    );
    assert_eq!(strip_sgr("\x1b]0;title\x07text"), "text");
    // DCS, SOS, PM and APC strings.
    assert_eq!(strip_sgr("a\x1bP1$r\x1b\\b"), "ab");
    assert_eq!(
        strip_sgr("a\x1b_x\x1b\\b\x1b^y\x1b\\c\x1bXz\x1b\\d"),
        "abcd"
    );
    // A charset, with its intermediate.
    assert_eq!(strip_sgr("\x1b(Bplain"), "plain");
    // An OSC never ended takes the rest.
    assert_eq!(strip_sgr("a\x1b]0;title"), "a");
}

fn config() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        cases: 1024,
        ..ProptestConfig::default()
    }
}

/// Terminal output: text and the sequences that matter here, mixed up.
fn output() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop_oneof![
        4 => "[a-z 🦀é]{1,8}".prop_map(String::into_bytes),
        2 => Just(b"\n".to_vec()),
        2 => Just(b"\r".to_vec()),
        1 => Just(b"\x08".to_vec()),
        1 => Just(b"\t".to_vec()),
        1 => (0u16..120, prop::sample::select(vec!['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'P', 'X', '@', 'd', 'm', 's', 'u']))
            .prop_map(|(n, action)| format!("\x1b[{n}{action}").into_bytes()),
        1 => (0u16..300, 0u16..300).prop_map(|(a, b)| format!("\x1b[{a};{b}H").into_bytes()),
        1 => "\x1b\\[[0-9;:]{0,12}m".prop_map(String::into_bytes),
        1 => Just(b"\x1b]0;title\x07".to_vec()),
        1 => Just(b"\x1b7".to_vec()),
        1 => Just(b"\x1b8".to_vec()),
        1 => Just(b"\x1bM".to_vec()),
        1 => Just(b"\x1bc".to_vec()),
        1 => prop::collection::vec(any::<u8>(), 1..8),
    ];
    prop::collection::vec(piece, 0..60).prop_map(|pieces| pieces.concat())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn the_screen_never_panics_and_its_changes_apply(
        input in output(),
        split in 1usize..64,
    ) {
        let mut screen = Screen::new();
        let mut all = Vec::new();
        for chunk in input.chunks(split) {
            if let Some(redraw) = screen.feed(chunk) {
                let from = usize::try_from(redraw.from).unwrap();
                prop_assert!(from <= all.len(), "a gap");
                all.truncate(from);
                all.extend(redraw.lines);
            }
            // Bounded: the window, and its lines.
            prop_assert!(screen.grid.rows.len() <= ROWS);
            for row in &screen.grid.rows {
                let rendered = render(&row.cells).len();
                prop_assert_eq!(row.cost, rendered, "a row's cost is its bytes");
                prop_assert!(rendered <= MOST_LINE);
            }
            prop_assert!(screen.grid.scrolled.is_empty());
            prop_assert!(screen.grid.row < ROWS);
        }
        // The same lines, however the input is split.
        prop_assert_eq!(&all, &lines(&input));
        for line in &all {
            prop_assert!(line.len() <= MOST_LINE, "{} bytes", line.len());
            // Every line is on its own: it ends in the default style.
            if let Some(last) = line.rfind('\x1b') {
                prop_assert!(line[last..].starts_with("\x1b[0m"), "{line:?}");
            }
            // And starts from it: its first code sets everything.
            if let Some(first) = line.find('\x1b') {
                prop_assert!(line[first..].starts_with("\x1b[0"), "{line:?}");
            }
        }
    }

    #[test]
    fn editing_a_full_line_never_takes_it_past_the_cap(
        edits in prop::collection::vec(
            (
                1usize..MOST_LINE,
                0u16..40,
                prop::sample::select(vec!["X", "P", "@", "K", "1K", "2K"]),
                0u8..8,
            ),
            1..12,
        ),
    ) {
        let mut screen = Screen::new();
        let mut all = Vec::new();
        let mut whole = full_red_line();
        apply(&mut all, screen.feed(&whole).unwrap());
        for (col, n, action, color) in edits {
            let edit = format!("\x1b[{col}G\x1b[3{color}m\x1b[{n}{action}");
            // The action, then a character at the cursor: each on its own.
            for part in [edit.as_str(), "y\x1b[0m"] {
                whole.extend_from_slice(part.as_bytes());
                if let Some(redraw) = screen.feed(part.as_bytes()) {
                    apply(&mut all, redraw);
                }
                for row in &screen.grid.rows {
                    let rendered = render(&row.cells).len();
                    prop_assert_eq!(row.cost, rendered);
                    prop_assert!(rendered <= MOST_LINE, "{} bytes after {:?}", rendered, part);
                }
            }
        }
        // Every change was given: piece by piece, it's what it is whole.
        prop_assert_eq!(all, lines(&whole));
    }

    #[test]
    fn a_line_of_coloured_text_strips_to_its_text(text in "[a-z ]{1,40}", color in 30u8..38) {
        let input = format!("\x1b[{color}m{text}\x1b[0m\n");
        let out = lines(input.as_bytes());
        prop_assert_eq!(out.len(), 1);
        prop_assert_eq!(strip_sgr(&out[0]), text);
    }
}

#[test]
fn a_huge_output_with_no_line_ends_stays_bounded() {
    let mut screen = Screen::new();
    let chunk = vec![b'x'; 64 * 1024];
    let mut lines = 0u64;
    for _ in 0..64 {
        if let Some(redraw) = screen.feed(&chunk) {
            lines = redraw.from + redraw.lines.len() as u64;
        }
        assert!(screen.grid.rows.len() <= ROWS);
        assert!(screen.grid.rows.iter().all(|row| row.cost <= MOST_LINE));
    }
    // 4 MiB of x, in lines of 16 KiB.
    assert_eq!(lines, 4 * 1024 * 1024 / MOST_LINE as u64);
}

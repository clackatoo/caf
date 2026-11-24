use std::path::Path;

use super::*;

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

/// The answer to `command`, which printed `text` and exited as `exit`.
fn ran(command: &str, text: &str, exit: Option<Exit>) -> String {
    answer(command, text, exit, false, None, None)
}

#[test]
fn timeouts_default_and_are_capped() {
    assert_eq!(timeout(None), Duration::from_secs(120));
    assert_eq!(timeout(Some(0)), Duration::from_secs(120));
    assert_eq!(timeout(Some(5000)), Duration::from_secs(5));
    assert_eq!(timeout(Some(600_000)), Duration::from_secs(600));
    assert_eq!(timeout(Some(u64::MAX)), Duration::from_secs(600));
}

#[test]
fn output_is_its_text_trimmed() {
    let out = lines(&[
        "",
        "  ",
        "\x1b[0;31merror\x1b[0m: no",
        "",
        "  at x",
        "",
        " ",
    ]);
    assert_eq!(output_text(&out), "error: no\n\n  at x");
    assert_eq!(output_text(&[]), "");
    assert_eq!(output_text(&lines(&["", "\x1b[0;1m \x1b[0m"])), "");
    // Leading spaces on the first line with text are kept.
    assert_eq!(output_text(&lines(&["   indented  "])), "   indented");
}

#[test]
fn a_command_that_succeeded_is_answered_its_output() {
    assert_eq!(ran("echo hi", "hi", Some(Exit::Code(0))), "hi");
    assert_eq!(ran("echo hi", "hi", None), "hi");
}

#[test]
fn a_command_with_no_output_says_so() {
    assert_eq!(
        ran("true", "", Some(Exit::Code(0))),
        "(Bash completed with no output)"
    );
    assert_eq!(
        answer(
            "true",
            "",
            Some(Exit::Code(0)),
            false,
            None,
            Some("terminal 7")
        ),
        "Ran in terminal 7\n(Bash completed with no output)"
    );
    assert_eq!(ran("false", "", Some(Exit::Code(1))), "Exit code 1");
}

#[test]
fn a_failure_says_how_it_ended_first_as_claude_code_does() {
    assert_eq!(
        ran("make", "oops", Some(Exit::Code(2))),
        "Exit code 2\noops"
    );
    assert_eq!(
        ran("sleep 9", "oops", Some(Exit::Signal(9))),
        "Killed by signal 9\noops"
    );
}

#[test]
fn searches_and_tests_that_find_nothing_havent_failed() {
    // Exit 1 from these says "no", not "error", as Claude Code reads it.
    for command in [
        "grep -r foo .",
        "rg foo",
        "egrep x f",
        "fgrep x f",
        "find . -name x",
        "diff a b",
        "test -f x",
        "[ -f x ]",
        "cd src && grep foo *.rs",
        "cat f | grep foo",
        "true; rg x",
    ] {
        assert_eq!(ran(command, "out", Some(Exit::Code(1))), "out", "{command}");
    }
    // Exit 2 is still an error.
    assert_eq!(
        ran("grep foo", "grep: x: No such file", Some(Exit::Code(2))),
        "Exit code 2\ngrep: x: No such file"
    );
    // It's the last command that counts.
    assert_eq!(
        ran("grep foo f && make", "out", Some(Exit::Code(1))),
        "Exit code 1\nout"
    );
    assert_eq!(
        ran("mygrep foo", "out", Some(Exit::Code(1))),
        "Exit code 1\nout"
    );
}

#[test]
fn a_stopped_command_says_it_was_aborted_after_its_output() {
    assert_eq!(
        answer(
            "sleep 9",
            "partial",
            Some(Exit::Signal(15)),
            true,
            None,
            None
        ),
        "partial\n<error>Command was aborted before completion</error>"
    );
    assert_eq!(
        answer("sleep 9", "", None, true, None, None),
        "<error>Command was aborted before completion</error>"
    );
}

#[test]
fn a_place_starts_the_answer() {
    assert_eq!(
        answer(
            "make",
            "hi",
            Some(Exit::Code(3)),
            false,
            None,
            Some("terminal 7")
        ),
        "Ran in terminal 7\nExit code 3\nhi"
    );
}

#[test]
fn output_too_large_is_saved_and_previewed_as_claude_code_does() {
    let text = "x".repeat(MOST_ANSWER);
    assert!(!too_large(&text));
    assert_eq!(ran("cat f", &text, None), text);
    // Lines of 100: the preview ends at the last line end in its first
    // 2,000 characters.
    let line = format!("{}\n", "y".repeat(99));
    let text = line.repeat(301);
    let text = text.trim_end();
    assert!(too_large(text));
    let path = Path::new("/run/caf/out/c/1.txt");
    let got = answer("cat f", text, Some(Exit::Code(1)), false, Some(path), None);
    let want = format!(
        "Exit code 1\n<persisted-output>\nOutput too large (29.4KB). Full output saved to: /run/caf/out/c/1.txt\n\nPreview (first 2KB):\n{}\n...\n</persisted-output>",
        line.repeat(20).trim_end()
    );
    assert_eq!(got, want);
}

#[test]
fn a_preview_with_no_line_end_late_enough_is_cut_at_2000_characters() {
    let text = "é".repeat(MOST_ANSWER + 1);
    let got = answer("cat f", &text, None, false, Some(Path::new("/o")), None);
    let preview = got.split("Preview (first 2KB):\n").nth(1).unwrap();
    let preview = preview.strip_suffix("\n...\n</persisted-output>").unwrap();
    assert_eq!(preview.chars().count(), PREVIEW);
    // A line end in the first half doesn't count.
    let text = format!("{}\n{}", "a".repeat(900), "b".repeat(MOST_ANSWER));
    let got = answer("cat f", &text, None, false, Some(Path::new("/o")), None);
    let preview = got.split("Preview (first 2KB):\n").nth(1).unwrap();
    let preview = preview.strip_suffix("\n...\n</persisted-output>").unwrap();
    assert_eq!(preview.chars().count(), PREVIEW);
}

#[test]
fn too_large_counts_characters_not_bytes() {
    let text = "é".repeat(MOST_ANSWER);
    assert!(!too_large(&text), "60,000 bytes, but 30,000 characters");
    let text = "é".repeat(MOST_ANSWER + 1);
    assert!(too_large(&text));
    // And its size is in characters, as Claude Code's is.
    let got = answer("cat f", &text, None, false, Some(Path::new("/o")), None);
    assert!(got.contains("Output too large (29.3KB)."), "{got}");
}

#[test]
fn output_too_large_and_unsaved_is_cut_as_claude_code_does() {
    let text = "a".repeat(MOST_ANSWER + 10);
    let got = ran("cat f", &text, None);
    let head =
        "<truncated-output>\nOutput too large (29.3KB). It could not be saved, so only the first ";
    assert!(got.starts_with(head), "{got}");
    let (said, rest) = got[head.len()..].split_once(" are shown").unwrap();
    assert_eq!(
        &rest[..rest.find("\n\n").unwrap()],
        "; the rest was dropped. If the tool can page or filter its results, call it again for the part you need."
    );
    let kept = rest[rest.find("\n\n").unwrap() + 2..]
        .strip_suffix("\n</truncated-output>")
        .unwrap();
    assert!(kept.chars().all(|c| c == 'a'));
    assert_eq!(said, format_size(kept.chars().count() as u64));
    // All of it fits in what's answered.
    assert!(got.chars().count() <= MOST_ANSWER);
}

#[test]
fn background_answers_read_as_claude_codes() {
    let path = Path::new("/run/caf/out/c/3.txt");
    assert_eq!(
        background(3, path),
        "Command running in background with ID: 3. Output is being written to: /run/caf/out/c/3.txt. You will be notified when it completes. To check interim output, use Read on that file path."
    );
    assert_eq!(
        timed_out(Duration::from_secs(120), 3, path),
        "Command did not complete within its 120s timeout and was moved to the background (ID: 3). Output is being written to: /run/caf/out/c/3.txt. You will be notified when it completes. To check interim output, use Read on that file path."
    );
    // Rounded, and never 0s.
    assert!(timed_out(Duration::from_millis(1499), 3, path).contains("within its 1s timeout"));
    assert!(timed_out(Duration::from_millis(100), 3, path).contains("within its 1s timeout"));
    assert_eq!(
        too_many_jobs(),
        "Too many background tasks running (8); stop one with TaskStop first."
    );
    assert_eq!(job_stopped(3), "Stopped task 3");
    assert_eq!(no_job("x7"), "No task x7 in this conversation");
}

#[test]
fn notices_read_as_claude_codes() {
    let path = Path::new("/o/2.txt");
    assert_eq!(
        notice(2, Some("toolu_1"), "npm test", JobEnd::Exited(0), path),
        "<task-notification>\n<task-id>2</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<output-file>/o/2.txt</output-file>\n<status>completed</status>\n<summary>Background command \"npm test\" completed (exit code 0)</summary>\n</task-notification>"
    );
    // Claude Code's says completed, with the code, whatever the code.
    assert_eq!(
        notice(2, None, "npm test", JobEnd::Exited(1), path),
        "<task-notification>\n<task-id>2</task-id>\n<output-file>/o/2.txt</output-file>\n<status>completed</status>\n<summary>Background command \"npm test\" completed (exit code 1)</summary>\n</task-notification>"
    );
    assert_eq!(
        notice(2, None, "npm test", JobEnd::Failed, path),
        "<task-notification>\n<task-id>2</task-id>\n<output-file>/o/2.txt</output-file>\n<status>failed</status>\n<summary>Background command \"npm test\" failed</summary>\n</task-notification>"
    );
    assert_eq!(
        notice(2, None, "npm test", JobEnd::Stopped, path),
        "<task-notification>\n<task-id>2</task-id>\n<output-file>/o/2.txt</output-file>\n<status>killed</status>\n<summary>Background command \"npm test\" was stopped</summary>\n</task-notification>"
    );
}

#[test]
fn a_job_ends_as_its_command_did() {
    assert_eq!(JobEnd::of(Some(Exit::Code(1)), false), JobEnd::Exited(1));
    assert_eq!(JobEnd::of(Some(Exit::Signal(9)), false), JobEnd::Failed);
    assert_eq!(JobEnd::of(None, false), JobEnd::Failed);
    assert_eq!(JobEnd::of(Some(Exit::Code(0)), true), JobEnd::Stopped);
}

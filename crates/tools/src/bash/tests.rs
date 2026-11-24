use std::path::Path;

use super::*;

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
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
    assert_eq!(answer("hi", Some(Exit::Code(0)), false, None, None), "hi");
    assert_eq!(answer("hi", None, false, None, None), "hi");
}

#[test]
fn a_command_with_no_output_says_so() {
    assert_eq!(
        answer("", Some(Exit::Code(0)), false, None, None),
        "(Bash completed with no output)"
    );
    assert_eq!(
        answer("", Some(Exit::Code(0)), false, None, Some("terminal 7")),
        "Ran in terminal 7\n(Bash completed with no output)"
    );
    assert_eq!(
        answer("", Some(Exit::Code(1)), false, None, None),
        "Exit code 1"
    );
}

#[test]
fn a_failure_adds_how_it_ended() {
    assert_eq!(
        answer("oops", Some(Exit::Code(2)), false, None, None),
        "oops\nExit code 2"
    );
    assert_eq!(
        answer("oops", Some(Exit::Signal(9)), false, None, None),
        "oops\nKilled by signal 9"
    );
}

#[test]
fn a_stopped_command_says_it_was_aborted() {
    assert_eq!(
        answer("partial", Some(Exit::Signal(15)), true, None, None),
        "partial\n<error>Command was aborted before completion</error>"
    );
    assert_eq!(
        answer("", None, true, None, None),
        "<error>Command was aborted before completion</error>"
    );
}

#[test]
fn a_place_starts_the_answer() {
    assert_eq!(
        answer("hi", Some(Exit::Code(3)), false, None, Some("terminal 7")),
        "Ran in terminal 7\nhi\nExit code 3"
    );
}

#[test]
fn output_too_large_is_saved_and_previewed() {
    let text = "x".repeat(MOST_ANSWER);
    assert!(!too_large(&text));
    assert_eq!(answer(&text, None, false, None, None), text);
    let text = "x".repeat(MOST_ANSWER + 1);
    assert!(too_large(&text));
    let path = Path::new("/run/caf/out/c/1.txt");
    let got = answer(&text, Some(Exit::Code(1)), false, Some(path), None);
    let want = format!(
        "Output too large (29.3KB). Full output saved to: /run/caf/out/c/1.txt\n\nPreview (first 2KB):\n{}\n...\nExit code 1",
        "x".repeat(PREVIEW)
    );
    assert_eq!(got, want);
}

#[test]
fn too_large_counts_characters_not_bytes() {
    let text = "é".repeat(MOST_ANSWER);
    assert!(!too_large(&text), "60,000 bytes, but 30,000 characters");
    let text = "é".repeat(MOST_ANSWER + 1);
    assert!(too_large(&text));
}

#[test]
fn a_preview_ends_at_a_character_boundary() {
    let text = format!("a{}", "€".repeat(MOST_ANSWER)); // 3 bytes each, after 1
    let got = answer(&text, None, false, Some(Path::new("/o")), None);
    let preview = got.split("Preview (first 2KB):\n").nth(1).unwrap();
    let preview = preview.strip_suffix("\n...").unwrap();
    assert!(preview.len() <= PREVIEW);
    assert!(preview.len() > PREVIEW - 3);
}

#[test]
fn output_too_large_and_unsaved_is_cut() {
    let text = "é".repeat(MOST_ANSWER + 10);
    let got = answer(&text, None, false, None, None);
    let (start, note) = got.split_once('\n').unwrap();
    assert_eq!(start.chars().count(), MOST_ANSWER);
    assert_eq!(
        note,
        "... (output too large: 58.6KB; only its start is shown)"
    );
}

#[test]
fn background_answers_read_as_claude_codes() {
    let path = Path::new("/run/caf/out/c/3.txt");
    assert_eq!(
        background(3, path),
        "Command running in background with ID: 3. Output is being written to: /run/caf/out/c/3.txt."
    );
    assert_eq!(
        timed_out(Duration::from_secs(120), 3, path),
        "Command still running after 2m; it continues in the background as job 3. Output is being written to: /run/caf/out/c/3.txt."
    );
    assert_eq!(
        too_many_jobs(),
        "Too many background tasks running (8); stop one with TaskStop first."
    );
    assert_eq!(job_stopped(3), "Stopped task 3");
    assert_eq!(no_job("x7"), "No task x7 in this conversation");
}

#[test]
fn durations_read_shortly() {
    let ms = Duration::from_millis;
    assert_eq!(format_duration(ms(0)), "0ms");
    assert_eq!(format_duration(ms(500)), "500ms");
    assert_eq!(format_duration(ms(1000)), "1s");
    assert_eq!(format_duration(ms(1500)), "1.5s");
    assert_eq!(format_duration(ms(59_999)), "60s");
    assert_eq!(format_duration(ms(60_000)), "1m");
    assert_eq!(format_duration(ms(150_000)), "2m 30s");
    assert_eq!(format_duration(ms(600_000)), "10m");
}

#[test]
fn notices_say_how_a_job_ended() {
    let path = Path::new("/o/2.txt");
    assert_eq!(
        notice(2, "npm test", JobEnd::Exited(0), path),
        "<task-notification>Task 2 (npm test) completed with exit code 0. Output: /o/2.txt</task-notification>"
    );
    assert_eq!(
        notice(2, "npm test", JobEnd::Failed, path),
        "<task-notification>Task 2 (npm test) failed. Output: /o/2.txt</task-notification>"
    );
    assert_eq!(
        notice(2, "npm test", JobEnd::Stopped, path),
        "<task-notification>Task 2 (npm test) was stopped. Output: /o/2.txt</task-notification>"
    );
}

#[test]
fn a_job_ends_as_its_command_did() {
    assert_eq!(JobEnd::of(Some(Exit::Code(1)), false), JobEnd::Exited(1));
    assert_eq!(JobEnd::of(Some(Exit::Signal(9)), false), JobEnd::Failed);
    assert_eq!(JobEnd::of(None, false), JobEnd::Failed);
    assert_eq!(JobEnd::of(Some(Exit::Code(0)), true), JobEnd::Stopped);
}

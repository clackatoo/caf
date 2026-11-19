//! `binary()`, as tests in other crates use it. A test program of its
//! own, so it runs after the others, never while they start the program.

use std::path::PathBuf;

#[test]
fn other_crates_find_the_program() {
    let found = caf_fake_agent::binary();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_caf-fake-agent"));
    assert_eq!(found.canonicalize().unwrap(), built.canonicalize().unwrap());
    let status = std::process::Command::new(&found)
        .arg("relative.jsonl")
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(caf_fake_agent::EXIT_BAD_SCRIPT));
}

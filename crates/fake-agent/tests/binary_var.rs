//! `binary()` given a program to use. A test program of its own, as it
//! sets an environment variable.

use std::path::PathBuf;

#[test]
fn a_program_named_in_the_environment_is_used_as_it_is() {
    let given =
        PathBuf::from(env!("CARGO_BIN_EXE_caf-fake-agent")).with_file_name("caf-fake-agent.d");
    // SAFETY: the only test in this program, before anything else reads
    // the environment.
    unsafe { std::env::set_var(caf_fake_agent::BIN_VAR, &given) };
    assert_eq!(caf_fake_agent::binary(), given);
}

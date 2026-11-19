//! `caf-fake-agent SCRIPT [--record RECORD]`: see the library's docs.

use std::path::Path;
use std::time::Duration;

use caf_fake_agent::{EXIT_BAD_SCRIPT, Script, run};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.as_slice() {
        // A process started by `spawn_holder`: it holds the pipes it was
        // given, then goes.
        [hold, secs] if hold == "--hold" => {
            std::thread::sleep(Duration::from_secs(secs.parse().unwrap_or(60)));
            0
        }
        [script] => start(script, None),
        [script, flag, record] if flag == "--record" => start(script, Some(record)),
        _ => {
            eprintln!("usage: caf-fake-agent SCRIPT [--record RECORD]");
            EXIT_BAD_SCRIPT
        }
    };
    std::process::exit(code);
}

fn start(script: &str, record: Option<&String>) -> i32 {
    match Script::load(Path::new(script)) {
        Ok(script) => run(&script, record.map(Path::new)),
        Err(err) => {
            eprintln!("caf-fake-agent: {err}");
            EXIT_BAD_SCRIPT
        }
    }
}

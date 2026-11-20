//! `caf-debugger`: see the library's docs.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::SystemTime;

use caf_debugger::server::{self, App};
use caf_debugger::{agents, fake, files, live, replay};

/// The port, unless `--port` says otherwise.
const PORT: u16 = 7320;

const USAGE: &str = "\
usage: caf-debugger [--port PORT] [--agents agents.toml]   run CAF live
       caf-debugger --replay FILE.jsonl [--port PORT]     replay a recording

The page is served on 127.0.0.1 only, at the URL printed, which holds this
launch's token. Port 7320 unless given; 0 picks a free one.";

/// What the command line asks for.
struct Args {
    port: u16,
    agents: Option<PathBuf>,
    replay: Option<PathBuf>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        port: PORT,
        agents: None,
        replay: None,
    };
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--port" => {
                let port = value("--port")?;
                parsed.port = port.parse().map_err(|_| format!("not a port: {port}"))?;
            }
            "--agents" => parsed.agents = Some(value("--agents")?.into()),
            "--replay" => parsed.replay = Some(value("--replay")?.into()),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if parsed.agents.is_some() && parsed.replay.is_some() {
        return Err("--agents is for running live, not replaying".into());
    }
    Ok(parsed)
}

#[tokio::main]
async fn main() -> ExitCode {
    log::set_logger(&Logger).expect("the only logger");
    log::set_max_level(log::LevelFilter::Info);
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(why) => {
            if why.is_empty() {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            eprintln!("caf-debugger: {why}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("caf-debugger: {why}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, args.port))
        .await
        .map_err(|error| format!("can't listen on 127.0.0.1:{}: {error}", args.port))?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let token = server::new_token();
    let (app, live) = match &args.replay {
        Some(path) => {
            let book = replay::load(path).map_err(|why| format!("{}: {why}", path.display()))?;
            println!("Replaying {} ({} steps).", path.display(), book.end());
            (App::new(book, port, token.clone()), None)
        }
        None => {
            let dir = files::state_dir()
                .ok_or("there's no $XDG_STATE_HOME or $HOME to keep recordings in")?;
            files::make(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
            let script =
                fake::write(&dir).map_err(|error| format!("the fake agent's script: {error}"))?;
            let agents = match &args.agents {
                Some(path) => {
                    agents::load(path).map_err(|why| format!("{}: {why}", path.display()))?
                }
                None => agents::found(fake_agent().as_deref(), &script),
            };
            if agents.is_empty() {
                return Err("no agents to offer: give some with --agents".into());
            }
            let recording = files::new_recording(&dir, SystemTime::now());
            let live = live::start(agents, Some(recording.clone()));
            let names: Vec<_> = live
                .caf
                .agents()
                .iter()
                .map(|name| name.0.as_str())
                .collect();
            println!("Running CAF with {}.", names.join(", "));
            println!("Recording to {}.", recording.display());
            let mut app = App::new(Arc::clone(&live.book), port, token.clone());
            app.live = Some((live.caf.clone(), Arc::clone(&live.rules)));
            app.recording = Some(recording.clone());
            (app, Some((live, recording)))
        }
    };
    if !app.dist.join("index.html").is_file() {
        log::warn!(
            "the app isn't built, so the page says how to build it: there's no {}",
            app.dist.display()
        );
    }
    println!("Open {}", server::url(port, &token));
    let serving = axum::serve(listener, server::router(Arc::new(app)));
    tokio::select! {
        served = serving => served.map_err(|error| error.to_string())?,
        _ = tokio::signal::ctrl_c() => {}
    }
    if let Some((live, recording)) = live {
        println!("Shutting CAF down.");
        live.caf.shutdown().await;
        live.running.await;
        println!("The run is recorded in {}.", recording.display());
    }
    Ok(())
}

/// The fake agent's program, if it can be found or built: see
/// [`caf_fake_agent::binary`], which panics if it can't. Its panic is
/// told as a warning, not printed as a crash.
fn fake_agent() -> Option<PathBuf> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let found = std::panic::catch_unwind(caf_fake_agent::binary);
    std::panic::set_hook(hook);
    match found {
        Ok(path) => Some(path),
        Err(panic) => {
            let why = (panic.downcast_ref::<String>().map(String::as_str))
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("it can't be found");
            // What it says says what to do.
            log::warn!("the fake agent isn't offered: {why}");
            None
        }
    }
}

/// Writes log lines to stderr.
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

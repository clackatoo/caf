//! `caf-debugger`: see the library's docs.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::SystemTime;

use caf_debugger::server::{self, App};
use caf_debugger::{agents, fake, files, live, replay};
use caf_driver_tokio::{Relay, UsageConfig};
use caf_save::FileStore;

/// Prints a line, as `println!` does, but one that can't be printed, as
/// when its terminal has closed, is left out: `println!` would panic, and
/// CAF wouldn't be shut down.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// [`say!`], to stderr.
macro_rules! tell {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// The port, unless `--port` says otherwise.
const PORT: u16 = 7320;

const USAGE: &str = "\
usage: caf-debugger [--port PORT] [--agents agents.toml] [--store DIR]
                    [--usage DIR] [--no-fetch-prices] [--fake-usage]
                    [--fake-limits]                                     run CAF live
       caf-debugger --replay FILE.jsonl [--port PORT]                   replay a recording

The page is served on 127.0.0.1 only, at the URL printed, which holds this
launch's token. Port 7320 unless given; 0 picks a free one.

Live, chats are saved in DIR, by default $XDG_DATA_HOME/caf-debugger/chats
(~/.local/share/caf-debugger/chats), and those saved before are listed, to
restore. Usage goes in the ledger in DIR, by default the one every CAF app
shares, $XDG_DATA_HOME/caf/usage (~/.local/share/caf/usage), and prices are
fetched from LiteLLM once a day unless --no-fetch-prices. The fake agent
says what its turns spent, made-up figures that count as real usage, only
with --fake-usage, and its account's limits, made up too and kept in the
same folder under its own profile, only with --fake-limits: send it
\"limit\" to see a turn refused for them. Their resets are made up as the
debugger starts, so they're gone, the five hours after three hours, the
weeks after four days, until it starts again.";

/// What the command line asks for.
struct Args {
    port: u16,
    agents: Option<PathBuf>,
    replay: Option<PathBuf>,
    store: Option<PathBuf>,
    usage: Option<PathBuf>,
    fetch_prices: bool,
    /// Whether the fake agent says what its turns spent.
    fake_usage: bool,
    /// Whether the fake agent says its account's limits.
    fake_limits: bool,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        port: PORT,
        agents: None,
        replay: None,
        store: None,
        usage: None,
        fetch_prices: true,
        fake_usage: false,
        fake_limits: false,
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
            "--store" => parsed.store = Some(value("--store")?.into()),
            "--usage" => parsed.usage = Some(value("--usage")?.into()),
            "--no-fetch-prices" => parsed.fetch_prices = false,
            "--fake-usage" => parsed.fake_usage = true,
            "--fake-limits" => parsed.fake_limits = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if parsed.agents.is_some() && parsed.replay.is_some() {
        return Err("--agents is for running live, not replaying".into());
    }
    if parsed.store.is_some() && parsed.replay.is_some() {
        return Err("--store is for running live, not replaying".into());
    }
    if parsed.usage.is_some() && parsed.replay.is_some() {
        return Err("--usage is for running live, not replaying".into());
    }
    if !parsed.fetch_prices && parsed.replay.is_some() {
        return Err("--no-fetch-prices is for running live, not replaying".into());
    }
    if parsed.fake_usage && parsed.replay.is_some() {
        return Err("--fake-usage is for running live, not replaying".into());
    }
    if parsed.fake_limits && parsed.replay.is_some() {
        return Err("--fake-limits is for running live, not replaying".into());
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    // First: run as CAF's relay, it does nothing else.
    caf_relay::relay_if_asked();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    runtime.block_on(debug())
}

/// The debugger, once it's known not to be a relay.
async fn debug() -> ExitCode {
    log::set_logger(&Logger).expect("the only logger");
    log::set_max_level(log::LevelFilter::Info);
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(why) => {
            if why.is_empty() {
                say!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            tell!("caf-debugger: {why}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            tell!("caf-debugger: {why}");
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
            say!("Replaying {} ({} steps).", path.display(), book.end());
            (App::new(book, port, token.clone()), None)
        }
        None => {
            let dir = files::state_dir()
                .ok_or("there's no $XDG_STATE_HOME or $HOME to keep recordings in")?;
            files::make(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
            let limits = args.fake_limits.then(SystemTime::now);
            let script = fake::write(&dir, args.fake_usage, limits)
                .map_err(|error| format!("the fake agent's script: {error}"))?;
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
            let store = match &args.store {
                Some(store) => Some(
                    std::path::absolute(store)
                        .map_err(|error| format!("{}: {error}", store.display()))?,
                ),
                None => FileStore::default_root("caf-debugger").or_else(|| {
                    log::warn!(
                        "there's no $XDG_DATA_HOME or $HOME to keep chats in: they aren't saved"
                    );
                    None
                }),
            };
            if let Some(store) = &store {
                files::check_store(store).map_err(|error| {
                    format!(
                        "can't keep chats in {}: {error}: give a folder it can write in with --store",
                        store.display()
                    )
                })?;
            }
            let usage = match &args.usage {
                Some(usage) => Some(UsageConfig::new(
                    std::path::absolute(usage)
                        .map_err(|error| format!("{}: {error}", usage.display()))?,
                )),
                None => UsageConfig::for_user().or_else(|| {
                    log::warn!(
                        "there's no $XDG_DATA_HOME or $HOME to keep usage in: it isn't kept"
                    );
                    None
                }),
            }
            .map(|mut usage| {
                usage.fetch_prices = args.fetch_prices;
                usage
            });
            let live = live::start(agents, Some(recording), store, usage, Relay::ThisProgram);
            let names: Vec<_> = live
                .caf
                .agents()
                .iter()
                .map(|name| name.0.as_str())
                .collect();
            say!("Running CAF with {}.", names.join(", "));
            match &live.recording {
                Some(path) => say!("Recording to {}.", path.display()),
                None => say!("Not recording: CAF couldn't make the file, as it says above."),
            }
            if let Some(store) = &live.store {
                say!("Keeping chats in {}.", store.display());
            }
            if let Some(usage) = &live.usage {
                say!("Keeping usage in {}.", usage.display());
            }
            let mut app = App::new(Arc::clone(&live.book), port, token.clone());
            app.live = Some((live.caf.clone(), Arc::clone(&live.rules)));
            app.recording.clone_from(&live.recording);
            (app, Some(live))
        }
    };
    if !app.dist.join("index.html").is_file() {
        log::warn!(
            "the app isn't built, so the page says how to build it: there's no {}",
            app.dist.display()
        );
    }
    // Listened for before it says it's serving, so a signal that comes
    // the moment it does is heard, never fatal.
    let mut stops =
        Stops::new().map_err(|error| format!("it can't listen for signals: {error}"))?;
    say!("Open {}", server::url(port, &token));
    let serving = axum::serve(listener, server::router(Arc::new(app)));
    tokio::select! {
        served = serving => served.map_err(|error| error.to_string())?,
        signal = stops.next() => say!("{signal}: shutting down. Again to stop at once."),
    }
    // A second signal doesn't wait for CAF.
    tokio::spawn(async move {
        let signal = stops.next().await;
        tell!("caf-debugger: {signal} again: stopping at once");
        std::process::exit(1);
    });
    if let Some(live) = live {
        live.caf.shutdown().await;
        live.running.await;
        if let Some(path) = &live.recording {
            say!("The run is recorded in {}.", path.display());
        }
    }
    Ok(())
}

/// The signals to stop on: Ctrl-C, `SIGTERM`, and `SIGHUP` as the
/// terminal closes. Each is caught from when these are made, so one that
/// comes before anything waits for it is kept, not fatal.
struct Stops {
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}

impl Stops {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
        })
    }

    /// The next signal to stop, named for saying so.
    async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.int.recv() => "Ctrl-C",
            _ = self.term.recv() => "SIGTERM",
            _ = self.hup.recv() => "SIGHUP",
        }
    }
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
            tell!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

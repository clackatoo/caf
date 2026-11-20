//! The kit against the real Claude adapter: opt-in. The chat test needs
//! a signed-in Claude and spends usage; the install test downloads
//! Claude's adapter from npm, and spends none.
//!
//! ```text
//! cargo test -p caf-driver-tokio --test live -- --ignored
//! ```
//!
//! `CAF_LIVE_CLAUDE` names the adapter, by default claude-agent-acp
//! 0.98.0 where clackatoo installs it. It runs on Haiku, the cheapest
//! model. `CAF_LIVE_RECORD` keeps the run's recording at that path.

use std::path::PathBuf;
use std::time::Duration;

use caf_core::agent::{AgentSpec, EnvChange, SpawnCommand};
use caf_core::change::Change;
use caf_core::chat::{ChatState, ItemKind, Turn, TurnLife, UserMessage};
use caf_core::host::Host;
use caf_core::profiles::{Builtin, ProfileChoice};
use caf_core::{Chat, OpenChat};
use caf_driver_tokio::{Caf, ChatHandle, Config, Update, Watch};

/// Long enough for a model to answer.
const PATIENCE: Duration = Duration::from_secs(180);

fn adapter() -> PathBuf {
    std::env::var_os("CAF_LIVE_CLAUDE").map_or_else(
        || {
            let home = PathBuf::from(std::env::var_os("HOME").expect("a home"));
            home.join(".local/share/clackatoo/agents/claude-0.98.0/claude-agent-acp/node_modules/.bin/claude-agent-acp")
        },
        PathBuf::from,
    )
}

fn words(turn: &Turn) -> String {
    (turn.items.iter())
        .filter_map(|item| match &item.kind {
            ItemKind::Words(words) => Some(words.as_str()),
            _ => None,
        })
        .collect()
}

async fn picture(chat: &ChatHandle) -> Chat {
    match chat.watch().next().await {
        Some(Update::Picture(picture)) => picture,
        other => panic!("a new watch starts with a picture, not {other:?}"),
    }
}

/// Waits until `done` is true of the chat, counting the words that came
/// in as changes.
async fn until(
    chat: &ChatHandle,
    watch: &mut Watch,
    streamed: &mut usize,
    what: &str,
    done: impl Fn(&Chat) -> bool,
) -> Chat {
    let waited = tokio::time::timeout(PATIENCE, async {
        loop {
            let now = picture(chat).await;
            if done(&now) {
                return now;
            }
            if let Some(Update::Changes(changes)) = watch.next().await {
                *streamed += (changes.iter())
                    .filter(|numbered| {
                        matches!(
                            numbered.change,
                            Change::Appended { .. } | Change::ItemAdded { .. }
                        )
                    })
                    .count();
            }
        }
    })
    .await;
    match waited {
        Ok(chat) => chat,
        Err(_) => panic!("gave up waiting for {what}: {:#?}", picture(chat).await),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a signed-in Claude, and spends usage"]
async fn claude_answers_and_stops() {
    let folder = std::env::temp_dir().join(format!("caf-live-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let recording = std::env::var_os("CAF_LIVE_RECORD")
        .map_or_else(|| folder.join("live.jsonl"), PathBuf::from);

    let mut command = SpawnCommand::new(adapter());
    command.env = vec![EnvChange::Set("ANTHROPIC_MODEL".into(), "haiku".into())];
    let claude = AgentSpec::command(
        "claude",
        command,
        ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
    );
    let mut config = Config::new(Host::new("caf-live-test", "0.0.0"), vec![claude]);
    config.record = Some(recording.clone());
    let (caf, running) = Caf::start(config);

    let chat = caf
        .open_chat(OpenChat::new("claude", &folder))
        .await
        .unwrap();
    let mut watch = chat.watch();
    assert!(matches!(watch.next().await, Some(Update::Picture(_))));
    let mut streamed = 0;
    chat.send(UserMessage::text("Reply with exactly the word pong."))
        .await
        .unwrap();
    let ponged = until(&chat, &mut watch, &mut streamed, "pong", |chat| {
        chat.turns
            .first()
            .is_some_and(|turn| turn.life != TurnLife::Waiting && turn.life != TurnLife::Running)
    })
    .await;
    assert_eq!(ponged.turns[0].life, TurnLife::Finished, "{ponged:#?}");
    assert!(
        words(&ponged.turns[0]).to_lowercase().contains("pong"),
        "{ponged:#?}"
    );
    assert!(streamed > 0, "the words came in as changes");

    chat.send(UserMessage::text(
        "Write a 1500-word story about a lighthouse keeper. Don't use any tools.",
    ))
    .await
    .unwrap();
    until(
        &chat,
        &mut watch,
        &mut streamed,
        "the story to start",
        |chat| chat.turns.get(1).is_some_and(|turn| words(turn).len() > 40),
    )
    .await;
    chat.stop().await.unwrap();
    let stopped = until(
        &chat,
        &mut watch,
        &mut streamed,
        "the story to stop",
        |chat| {
            chat.turns.get(1).is_some_and(|turn| {
                turn.life != TurnLife::Running && turn.life != TurnLife::Stopping
            })
        },
    )
    .await;
    assert_eq!(stopped.turns[1].life, TurnLife::Stopped, "{stopped:#?}");
    assert!(matches!(stopped.state, ChatState::Open { .. }));

    tokio::time::timeout(PATIENCE, async {
        caf.shutdown().await;
        running.await;
    })
    .await
    .expect("shutting down");
    println!("recorded to {}", recording.display());
    if std::env::var_os("CAF_LIVE_RECORD").is_none() {
        let _ = std::fs::remove_dir_all(&folder);
    }
}

/// Installs Claude's adapter for real, as its profile's recipe says, into
/// a folder of its own under `TMPDIR`, then asks it whether it's signed
/// in. It never signs in and opens no chat, so it spends no usage, but it
/// downloads about 600 MB from npm, and reads the machine's own Claude
/// sign-in (`~/.claude`) without changing it.
///
/// ```text
/// cargo test -p caf-driver-tokio --test live installing -- --ignored
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "downloads Claude's adapter from npm"]
async fn installing_claude_for_real_then_asking_whether_its_signed_in() {
    use caf_core::agent::{InstallState, SignInState};
    use caf_core::ids::AgentName;

    let installs = std::env::temp_dir().join(format!("caf-live-install-{}", std::process::id()));
    let recording = installs.with_extension("jsonl");
    let mut config = Config::new(
        Host::new("caf-live", "1.0.0"),
        vec![AgentSpec::installed("claude", Builtin::ClaudeAgentAcp)],
    );
    config.installs = Some(installs.clone());
    config.record = Some(recording.clone());
    let (caf, running) = Caf::<caf_core::NoAppItems>::start(config);
    let claude = AgentName("claude".into());
    let mut agents = caf.watch_agents();
    let surveyed = async {
        while let Some(statuses) = agents.next().await {
            if statuses[0].install != InstallState::Unknown {
                return statuses;
            }
        }
        panic!("CAF ended")
    };
    let statuses = tokio::time::timeout(PATIENCE, surveyed)
        .await
        .expect("a survey");
    assert!(
        matches!(statuses[0].install, InstallState::Missing { .. }),
        "this machine can install it: {statuses:?}"
    );

    caf.install(&claude).await.unwrap();
    let installed = async {
        while let Some(statuses) = agents.next().await {
            match &statuses[0].install {
                InstallState::Installed { version } => {
                    assert_eq!(version, "0.98.0");
                }
                InstallState::Cannot(why) => panic!("the install failed: {why:?}"),
                _ => continue,
            }
            // Its status command answers once it's installed.
            if !matches!(statuses[0].sign_in, SignInState::Unknown) {
                return statuses;
            }
        }
        panic!("CAF ended")
    };
    let statuses = tokio::time::timeout(Duration::from_secs(20 * 60), installed)
        .await
        .expect("installed, and asked whether it's signed in, within 20 minutes");
    eprintln!("Claude, installed: {:?}", statuses[0]);
    let adapter = installs.join("claude-0.98.0/node_modules/.bin/claude-agent-acp");
    assert!(adapter.exists(), "{} is there", adapter.display());
    let names: Vec<String> = std::fs::read_dir(&installs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name != ".lock")
        .collect();
    assert_eq!(names, ["claude-0.98.0"], "nothing half-built is left");

    caf.shutdown().await;
    running.await;
    let text = std::fs::read_to_string(&recording).unwrap();
    assert!(
        !text.contains("\"email\"") && !text.contains("orgName"),
        "nothing but how it's signed in is recorded"
    );
    let _ = std::fs::remove_dir_all(&installs);
    let _ = std::fs::remove_file(&recording);
}

//! Patched adapters: a new patch level made from an earlier one on the
//! disk, with nothing downloaded and nobody asked, an install as any other
//! when it can't be, and earlier levels removed as older versions are.

use serde_json::{Value, json};

use super::harness::{EPOCH, Script, assert_has, host, sent, with};
use super::setup::{found, notes, npm_init, one, ticket_of};
use crate::agent::InstallFailure;
use crate::core::OpenChat;
use crate::event::SetupEvent;
use crate::install::Patch;
use crate::profiles::tests::NPM_PATCH;
use crate::time::Wall;

/// A day, in milliseconds.
const DAY: i64 = 24 * 60 * 60 * 1000;

/// The npm agent's recipe patched at a level, on this thread, until it's
/// dropped. Made before the script, so the script's replay, as it's
/// dropped, sees it too.
struct Patched;

impl Patched {
    fn at(level: u32) -> Self {
        NPM_PATCH.set(Some(Patch::new(level, Vec::new())));
        Self
    }
}

impl Drop for Patched {
    fn drop(&mut self) {
        NPM_PATCH.set(None);
    }
}

impl Script {
    /// Checks what's installed, and the survey finds `installed`, each
    /// last used `used`. Returns what the answer gave out.
    fn found(&mut self, installed: &[&str], used: Wall) -> Vec<Value> {
        let out = self.check_agents();
        let SetupEvent::Surveyed {
            root,
            node,
            npm,
            platform,
            ..
        } = found(&[])
        else {
            unreachable!()
        };
        self.setup(
            ticket_of(&out, "survey"),
            SetupEvent::Surveyed {
                root,
                installed: installed.iter().map(|&each| each.to_owned()).collect(),
                node,
                npm,
                platform,
                used: (installed.iter())
                    .map(|&each| (each.to_owned(), used))
                    .collect(),
            },
        )
    }
}

/// The npm agent's install state, as JSON.
fn installing() -> Value {
    json!({ "type": "Installing", "value": { "done": 0, "size": 40_000_000 } })
}

/// The npm agent's install state, not installed.
fn missing() -> Value {
    json!({ "type": "Missing", "value": { "size": 40_000_000 } })
}

#[test]
fn a_new_patch_level_is_prepared_from_an_earlier_one_with_nobody_asked() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let out = script.found(
        &[
            "test-npm-1.9.0+caf.4",
            "test-npm-2.0.0",
            "test-npm-2.0.0+caf.1",
            // Another program's later level, and a patch this CAF doesn't
            // know: never prepared from.
            "test-npm-2.0.0+caf.3",
            "test-npm-2.0.0+caf.x",
        ],
        Wall(EPOCH),
    );
    assert!(with(&out, "install").is_empty(), "{out:#?}");
    let prepare = one(&out, "prepare");
    assert_eq!(
        prepare,
        json!({ "prepare": prepare["prepare"], "folder": "test-npm-2.0.0+caf.2", "from": "test-npm-2.0.0+caf.1", "level": 2, "private": "test-npm-private" })
    );
    let prepare = prepare["prepare"].as_u64().expect("a ticket");
    // Every agent of its profile, shown installing, with nothing asked.
    for agent in ["npm-agent", "npm-twin"] {
        assert_eq!(script.agent_status(agent)["install"], installing());
    }
    let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
    let chat = chat.expect("opened");
    assert!(with(&out, "start").is_empty());
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" } }),
    );
    // A host installing it meanwhile adds nothing.
    let (done, out) = script.install("npm-twin");
    assert_eq!(done, Ok(()));
    assert!(out.is_empty(), "{out:#?}");
    let out = script.setup(prepare, SetupEvent::Installed);
    for agent in ["npm-agent", "npm-twin"] {
        assert_eq!(
            script.agent_status(agent)["install"],
            json!({ "type": "Installed", "value": { "version": "2.0.0+caf.2" } })
        );
    }
    // The chat starts from the new level.
    let start = one(&out, "start");
    assert_eq!(start["hold"], "/caf/agents/test-npm-2.0.0+caf.2");
    assert_eq!(
        start["program"],
        "/caf/agents/test-npm-2.0.0+caf.2/node_modules/.bin/test-npm-acp"
    );
    sent(&out, "initialize");
}

#[test]
fn an_installed_patch_level_is_never_prepared() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let out = script.found(
        &["test-npm-2.0.0+caf.1", "test-npm-2.0.0+caf.2"],
        Wall(EPOCH),
    );
    assert!(with(&out, "prepare").is_empty(), "{out:#?}");
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Installed", "value": { "version": "2.0.0+caf.2" } })
    );
}

#[test]
fn a_patch_level_with_no_earlier_one_of_its_upstream_version_is_missing() {
    let _patched = Patched::at(1);
    let mut script = Script::new(host());
    let out = script.found(
        &["test-npm-1.9.0+caf.1", "test-npm-2.0.0+caf.2"],
        Wall(EPOCH),
    );
    assert!(with(&out, "prepare").is_empty(), "{out:#?}");
    assert_eq!(script.agent_status("npm-agent")["install"], missing());
}

#[test]
fn a_patch_level_that_cant_be_prepared_is_installed_as_any_other_once_asked() {
    for failed in [
        SetupEvent::InstallFailed(InstallFailure::Patch(
            "node_modules/test-npm-acp/dist/agent.js isn't the file this patch was written for"
                .into(),
        )),
        SetupEvent::Failed("the work panicked".into()),
    ] {
        let _patched = Patched::at(2);
        let mut script = Script::new(host());
        let out = script.found(&["test-npm-2.0.0+caf.1"], Wall(EPOCH));
        let prepare = ticket_of(&out, "prepare");
        let chat = (script.open(OpenChat::new("npm-agent", "/work")).0).expect("opened");
        let turn = script.send(chat, "hello").0.expect("sent");
        let out = script.setup(prepare, failed);
        assert_eq!(with(&out, "report")[0]["report"], "Warn");
        for agent in ["npm-agent", "npm-twin"] {
            assert_eq!(script.agent_status(agent)["install"], missing());
        }
        // Its message waits on, for the host to ask the user.
        let whole = script.chat(chat);
        assert_has(
            &whole,
            &json!({ "state": { "type": "WaitingForAgent" }, "turns": [{ "id": turn.0, "life": { "type": "Waiting" } }] }),
        );
        assert!(notes(&whole).is_empty(), "{:?}", notes(&whole));
        // Never prepared again: a survey finds it missing.
        let out = script.found(&["test-npm-2.0.0+caf.1"], Wall(EPOCH));
        assert!(with(&out, "prepare").is_empty(), "{out:#?}");
        assert_eq!(script.agent_status("npm-agent")["install"], missing());
        // Installed when asked, downloaded as any other.
        let out = script.install("npm-agent").1;
        assert!(with(&out, "prepare").is_empty(), "{out:#?}");
        let install = one(&out, "install");
        assert_eq!(install["folder"], "test-npm-2.0.0+caf.2");
        let out = script.setup(
            install["install"].as_u64().expect("a ticket"),
            SetupEvent::Installed,
        );
        one(&out, "start");
    }
}

#[test]
fn a_patch_level_whose_preparing_was_stopped_is_prepared_when_installed() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let prepare = ticket_of(
        &script.found(&["test-npm-2.0.0+caf.1"], Wall(EPOCH)),
        "prepare",
    );
    let (done, out) = script.stop_install("npm-agent");
    assert_eq!(done, Ok(()));
    assert_eq!(ticket_of(&out, "stop"), prepare);
    assert_eq!(script.agent_status("npm-agent")["install"], missing());
    script.setup(prepare, SetupEvent::InstallFailed(InstallFailure::Stopped));
    assert_eq!(script.agent_status("npm-agent")["install"], missing());
    // Installing it prepares it again: still nothing to download.
    let out = script.install("npm-agent").1;
    assert!(with(&out, "install").is_empty(), "{out:#?}");
    assert_eq!(one(&out, "prepare")["from"], "test-npm-2.0.0+caf.1");
    assert_eq!(script.agent_status("npm-agent")["install"], installing());
}

#[test]
fn earlier_patch_levels_are_removed_as_older_versions_are() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    script.found(
        &[
            "test-npm-1.9.0+caf.7",
            "test-npm-2.0.0",
            "test-npm-2.0.0+caf.1",
            "test-npm-2.0.0+caf.2",
            // Another program's later level, and a patch this CAF doesn't
            // know: never older.
            "test-npm-2.0.0+caf.3",
            "test-npm-2.0.0+caf.x",
            "test-npm-2.1.0",
        ],
        Wall(EPOCH - 30 * DAY),
    );
    let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
    chat.expect("opened");
    let proc = one(&out, "start")["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &npm_init());
    let mut removed: Vec<Value> = (with(&out, "remove").into_iter())
        .map(|each| each["folder"].clone())
        .collect();
    removed.sort_by_key(ToString::to_string);
    assert_eq!(
        removed,
        [
            "test-npm-1.9.0+caf.7",
            "test-npm-2.0.0",
            "test-npm-2.0.0+caf.1"
        ]
    );
}

#[test]
fn a_prepare_the_host_asked_for_that_fails_is_downloaded_at_once() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let unasked = ticket_of(
        &script.found(&["test-npm-2.0.0+caf.1"], Wall(EPOCH)),
        "prepare",
    );
    assert_eq!(script.stop_install("npm-agent").0, Ok(()));
    script.setup(unasked, SetupEvent::InstallFailed(InstallFailure::Stopped));
    // The host asks: still nothing to download, so it's prepared.
    let asked = ticket_of(&script.install("npm-agent").1, "prepare");
    let chat = (script.open(OpenChat::new("npm-agent", "/work")).0).expect("opened");
    let turn = script.send(chat, "hello").0.expect("sent");
    let out = script.setup(
        asked,
        SetupEvent::InstallFailed(InstallFailure::Patch(
            "node_modules/test-npm-acp/dist/agent.js isn't the file this patch was written for"
                .into(),
        )),
    );
    // The user agreed to install it: it's downloaded at once, never
    // prepared again, and the agent goes on installing.
    assert!(with(&out, "prepare").is_empty(), "{out:#?}");
    let install = one(&out, "install");
    assert_eq!(
        install,
        json!({ "install": install["install"], "folder": "test-npm-2.0.0+caf.2", "how": "Npm", "private": "test-npm-private", "size": 40_000_000 })
    );
    for agent in ["npm-agent", "npm-twin"] {
        assert_eq!(script.agent_status(agent)["install"], installing());
    }
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" }, "turns": [{ "id": turn.0, "life": { "type": "Waiting" } }] }),
    );
    let out = script.setup(
        install["install"].as_u64().expect("a ticket"),
        SetupEvent::Installed,
    );
    one(&out, "start");
}

#[test]
fn an_install_asked_for_before_any_survey_prepares_once_the_survey_says_it_can() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let out = script.install("npm-agent").1;
    // Whether an earlier level is there waits for the survey.
    let survey = ticket_of(&out, "survey");
    assert!(with(&out, "install").is_empty(), "{out:#?}");
    assert!(with(&out, "prepare").is_empty(), "{out:#?}");
    for agent in ["npm-agent", "npm-twin"] {
        assert_eq!(script.agent_status(agent)["install"], installing());
    }
    // Asking again, for either, adds nothing.
    assert!(script.install("npm-twin").1.is_empty());
    let SetupEvent::Surveyed {
        root,
        node,
        npm,
        platform,
        ..
    } = found(&[])
    else {
        unreachable!()
    };
    let out = script.setup(
        survey,
        SetupEvent::Surveyed {
            root,
            installed: vec!["test-npm-2.0.0+caf.1".into()],
            node,
            npm,
            platform,
            used: Vec::new(),
        },
    );
    assert!(with(&out, "install").is_empty(), "{out:#?}");
    let prepare = one(&out, "prepare");
    assert_eq!(prepare["from"], "test-npm-2.0.0+caf.1");
    assert_eq!(script.agent_status("npm-agent")["install"], installing());
    // Asked for: if it fails, it's downloaded.
    let out = script.setup(
        prepare["prepare"].as_u64().expect("a ticket"),
        SetupEvent::Failed("the work panicked".into()),
    );
    one(&out, "install");
}

#[test]
fn an_install_asked_for_before_any_survey_downloads_once_the_survey_says_it_must() {
    let _patched = Patched::at(2);
    // Nothing earlier there.
    let mut script = Script::new(host());
    let survey = ticket_of(&script.install("npm-agent").1, "survey");
    let out = script.setup(survey, found(&["test-npm-1.9.0+caf.1"]));
    assert!(with(&out, "prepare").is_empty(), "{out:#?}");
    assert_eq!(one(&out, "install")["folder"], "test-npm-2.0.0+caf.2");
    assert_eq!(script.agent_status("npm-agent")["install"], installing());
    // A survey that can't look: downloaded, as before there were patches.
    let mut script = Script::new(host());
    let survey = ticket_of(&script.install("npm-agent").1, "survey");
    let out = script.setup(survey, SetupEvent::Failed("no root".into()));
    assert_eq!(one(&out, "install")["folder"], "test-npm-2.0.0+caf.2");
    // Installed already: nothing to do.
    let mut script = Script::new(host());
    let survey = ticket_of(&script.install("npm-agent").1, "survey");
    let out = script.setup(survey, found(&["test-npm-2.0.0+caf.2"]));
    assert!(with(&out, "install").is_empty() && with(&out, "prepare").is_empty());
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Installed", "value": { "version": "2.0.0+caf.2" } })
    );
    // The machine can't: it says why, and nothing's started.
    let mut script = Script::new(host());
    let survey = ticket_of(&script.install("npm-agent").1, "survey");
    let SetupEvent::Surveyed {
        root,
        installed,
        platform,
        used,
        ..
    } = found(&[])
    else {
        unreachable!()
    };
    let out = script.setup(
        survey,
        SetupEvent::Surveyed {
            root,
            installed,
            node: None,
            npm: true,
            platform,
            used,
        },
    );
    assert!(with(&out, "install").is_empty() && with(&out, "prepare").is_empty());
    assert_eq!(
        script.agent_status("npm-agent")["install"]["value"]["type"],
        "Node"
    );
}

#[test]
fn an_install_asked_for_before_any_survey_can_be_stopped() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let survey = ticket_of(&script.install("npm-agent").1, "survey");
    let (done, out) = script.stop_install("npm-twin");
    assert_eq!(done, Ok(()));
    assert!(out.is_empty(), "{out:#?}");
    for agent in ["npm-agent", "npm-twin"] {
        assert_eq!(script.agent_status(agent)["install"], missing());
    }
    let out = script.setup(survey, found(&[]));
    assert!(with(&out, "install").is_empty(), "{out:#?}");
    assert_eq!(script.agent_status("npm-agent")["install"], missing());
}

#[test]
fn a_prepare_the_host_asks_for_while_it_runs_is_downloaded_if_it_fails() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let prepare = ticket_of(
        &script.found(&["test-npm-2.0.0+caf.1"], Wall(EPOCH)),
        "prepare",
    );
    // Started by the survey, then asked for: the user has agreed now.
    let (done, out) = script.install("npm-twin");
    assert_eq!(done, Ok(()));
    assert!(out.is_empty(), "{out:#?}");
    let out = script.setup(prepare, SetupEvent::Failed("the work panicked".into()));
    assert_eq!(one(&out, "install")["folder"], "test-npm-2.0.0+caf.2");
    assert_eq!(script.agent_status("npm-agent")["install"], installing());
}

#[test]
fn a_patch_level_the_user_stopped_isnt_prepared_again_until_asked() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let prepare = ticket_of(
        &script.found(&["test-npm-2.0.0+caf.1"], Wall(EPOCH)),
        "prepare",
    );
    assert_eq!(script.stop_install("npm-agent").0, Ok(()));
    script.setup(prepare, SetupEvent::InstallFailed(InstallFailure::Stopped));
    // Each survey after leaves it as the user did.
    for _ in 0..2 {
        let out = script.found(&["test-npm-2.0.0+caf.1"], Wall(EPOCH));
        assert!(with(&out, "prepare").is_empty(), "{out:#?}");
        assert_eq!(script.agent_status("npm-agent")["install"], missing());
    }
    // Until the host asks: then, and at each survey after, it's as before.
    let asked = ticket_of(&script.install("npm-agent").1, "prepare");
    script.setup(asked, SetupEvent::Installed);
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Installed", "value": { "version": "2.0.0+caf.2" } })
    );
}

#[test]
fn an_install_waiting_for_the_survey_goes_as_caf_shuts_down() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let survey = ticket_of(&script.install("npm-agent").1, "survey");
    script.shutdown();
    for agent in ["npm-agent", "npm-twin"] {
        assert_eq!(script.agent_status(agent)["install"], missing());
    }
    let out = script.setup(survey, found(&["test-npm-2.0.0+caf.1"]));
    assert!(
        with(&out, "install").is_empty() && with(&out, "prepare").is_empty(),
        "{out:#?}"
    );
}

#[test]
fn an_install_waiting_for_the_survey_waits_for_the_latest() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let first = ticket_of(&script.install("npm-agent").1, "survey");
    let latest = ticket_of(&script.check_agents(), "survey");
    // The first is stale: nothing's decided by it.
    let out = script.setup(first, found(&[]));
    assert!(
        with(&out, "install").is_empty() && with(&out, "prepare").is_empty(),
        "{out:#?}"
    );
    assert_eq!(script.agent_status("npm-agent")["install"], installing());
    let out = script.setup(latest, found(&["test-npm-2.0.0+caf.1"]));
    assert_eq!(one(&out, "prepare")["from"], "test-npm-2.0.0+caf.1");
}

#[test]
fn a_chat_waiting_for_an_install_the_survey_finds_done_starts() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let survey = ticket_of(&script.install("npm-agent").1, "survey");
    let chat = (script.open(OpenChat::new("npm-agent", "/work")).0).expect("opened");
    let turn = script.send(chat, "hello").0.expect("sent");
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" }, "turns": [{ "id": turn.0, "life": { "type": "Waiting" } }] }),
    );
    let out = script.setup(survey, found(&["test-npm-2.0.0+caf.2"]));
    let start = one(&out, "start");
    assert_eq!(start["hold"], "/caf/agents/test-npm-2.0.0+caf.2");
}

#[test]
fn an_install_waiting_for_the_survey_that_cant_happen_stops_the_messages_waiting() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    let survey = ticket_of(&script.install("npm-agent").1, "survey");
    let chat = (script.open(OpenChat::new("npm-agent", "/work")).0).expect("opened");
    let turn = script.send(chat, "hello").0.expect("sent");
    let SetupEvent::Surveyed {
        root,
        installed,
        platform,
        used,
        ..
    } = found(&[])
    else {
        unreachable!()
    };
    let out = script.setup(
        survey,
        SetupEvent::Surveyed {
            root,
            installed,
            node: Some(18),
            npm: true,
            platform,
            used,
        },
    );
    assert!(with(&out, "install").is_empty(), "{out:#?}");
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Cannot", "value": { "type": "Node", "value": { "needs": 22, "has": 18 } } })
    );
    // As for an install that failed: the user asked, it couldn't be, and
    // the agent's status says why.
    let whole = script.chat(chat);
    assert_has(
        &whole,
        &json!({ "state": { "type": "WaitingForAgent" }, "turns": [{ "id": turn.0, "life": { "type": "Stopped" } }, {}] }),
    );
    assert_eq!(notes(&whole), ["InstallFailed"]);
}

#[test]
fn learning_while_an_install_waits_for_the_survey_says_caf_hasnt_looked() {
    let _patched = Patched::at(2);
    let mut script = Script::new(host());
    assert_eq!(script.install("npm-agent").0, Ok(()));
    let (done, out) = script.learn_settings("npm-agent");
    done.expect("taken");
    assert!(with(&out, "start").is_empty());
    assert_eq!(
        script.agent_status("npm-agent")["settings"],
        json!({ "type": "Failed", "value": "CAF hasn't looked whether the agent is installed yet" })
    );
}

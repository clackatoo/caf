//! Installing agents and signing in to them, with the two made-up agents
//! of different recipes (`npm-agent` and `dl-agent`) and Claude, started
//! by the host's own command.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host, sent, with};
use super::{CHAT, PROC, claude_init};
use crate::agent::InstallFailure;
use crate::core::{OpenChat, Refused};
use crate::event::SetupEvent;
use crate::install::{Platform, SignedInAs};

/// Where the setup driver keeps installs, in these tests.
const ROOT: &str = "/caf/agents";

/// The one directive in `out` with `key`, such as `"install"`.
#[track_caller]
fn one(out: &[Value], key: &str) -> Value {
    let found = with(out, key);
    assert_eq!(found.len(), 1, "one {key} in {out:#?}");
    found.into_iter().next().unwrap_or_default()
}

/// The ticket of the one directive in `out` with `key`.
#[track_caller]
fn ticket_of(out: &[Value], key: &str) -> u64 {
    one(out, key)[key].as_u64().expect("a ticket")
}

/// What a survey of a Linux x86_64 machine with Node 22 and npm finds,
/// with `installed`.
fn found(installed: &[&str]) -> SetupEvent {
    found_on(installed, Some(22), true, "x86_64")
}

fn found_on(installed: &[&str], node: Option<u32>, npm: bool, arch: &str) -> SetupEvent {
    SetupEvent::Surveyed {
        root: ROOT.into(),
        installed: installed.iter().map(|&each| each.to_owned()).collect(),
        node,
        npm,
        platform: Platform {
            os: "linux".into(),
            arch: arch.into(),
        },
    }
}

impl Script {
    /// Checks what's installed, and the survey finds `installed`. Returns
    /// what the answer gave out.
    fn surveyed(&mut self, installed: &[&str]) -> Vec<Value> {
        let out = self.check_agents();
        let survey = ticket_of(&out, "survey");
        self.setup(survey, found(installed))
    }
}

/// What the npm agent's adapter answers `initialize` with.
fn npm_init() -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": { "loadSession": true },
        "agentInfo": { "name": "test-npm-acp", "version": "2.0.0" }
    })
}

/// A sign-in state, as JSON.
fn signed_in(how: &str) -> Value {
    json!({ "type": "SignedIn", "value": { "type": how } })
}

/// The notes in `chat`, in order.
fn notes(chat: &Value) -> Vec<Value> {
    (chat["turns"].as_array().into_iter().flatten())
        .filter(|turn| turn["starter"]["type"] == "Caf")
        .flat_map(|turn| turn["items"].as_array().cloned().unwrap_or_default())
        .map(|item| item["kind"]["value"]["type"].clone())
        .collect()
}

#[test]
fn every_agent_stands_somewhere_in_the_order_offered_and_a_survey_says_where() {
    let mut script = Script::new(host());
    assert_eq!(
        script.agents(),
        json!([
            { "name": "claude", "install": { "type": "Host" }, "sign_in": { "type": "Unknown" }, "can_sign_in": false },
            { "name": "plain", "install": { "type": "Host" }, "sign_in": { "type": "Unknown" }, "can_sign_in": false },
            { "name": "npm-agent", "install": { "type": "Unknown" }, "sign_in": { "type": "Unknown" }, "can_sign_in": false },
            { "name": "dl-agent", "install": { "type": "Unknown" }, "sign_in": { "type": "Unknown" }, "can_sign_in": false },
        ])
    );
    let out = script.surveyed(&["dl-agent-1.4.0", "someone-else-1.0.0"]);
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Missing", "value": { "size": 40_000_000 } } }),
    );
    assert_has(
        &script.agent_status("dl-agent"),
        &json!({ "install": { "type": "Installed", "value": { "version": "1.4.0" } } }),
    );
    // The installed agent's status command runs, from its install, with
    // its private folder.
    assert_eq!(
        one(&out, "status"),
        json!({
            "status": 1001,
            "command": {
                "program": "/caf/agents/dl-agent-1.4.0/bin/test-download",
                "args": ["auth", "status"],
                "env": ["TEST_HOME=/caf/agents/dl-agent-private"],
            },
            "signed_in": "/signedIn",
            "how": ["/via", [["key", { "type": "ApiKey" }], ["account", { "type": "Account" }]]],
        })
    );
    script.setup(1001, SetupEvent::Status(Some(SignedInAs::Account)));
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        signed_in("Account")
    );
    script.check_agents();
    script.setup(1002, SetupEvent::Status(None));
    script.setup(1003, found(&["dl-agent-1.4.0"]));
    // A status command whose agent is signed out says so.
    let out = script.check_agents();
    let survey = ticket_of(&out, "survey");
    let out = script.setup(survey, found(&["dl-agent-1.4.0"]));
    script.setup(ticket_of(&out, "status"), SetupEvent::Status(None));
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        json!({ "type": "SignedOut" })
    );
}

#[test]
fn a_survey_says_what_the_machine_lacks() {
    let mut script = Script::new(host());
    let out = script.check_agents();
    script.setup(
        ticket_of(&out, "survey"),
        found_on(&[], None, true, "x86_64"),
    );
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Cannot", "value": { "type": "Node", "value": { "needs": 22, "has": null } } } }),
    );
    let out = script.check_agents();
    script.setup(
        ticket_of(&out, "survey"),
        found_on(&[], Some(20), true, "x86_64"),
    );
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Cannot", "value": { "type": "Node", "value": { "needs": 22, "has": 20 } } } }),
    );
    assert_eq!(script.install("npm-agent").0, Err(Refused::NotInstallable));
    let out = script.check_agents();
    script.setup(
        ticket_of(&out, "survey"),
        found_on(&[], Some(24), false, "riscv64"),
    );
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Cannot", "value": { "type": "Npm" } } }),
    );
    // The downloaded agent has no build for this machine, and needs
    // neither Node nor npm.
    assert_has(
        &script.agent_status("dl-agent"),
        &json!({ "install": { "type": "Cannot", "value": { "type": "Platform" } } }),
    );
    assert_eq!(script.install("dl-agent").0, Err(Refused::NotInstallable));
    // Once the user has fixed it, a new look finds it can be installed.
    let out = script.check_agents();
    script.setup(ticket_of(&out, "survey"), found(&[]));
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Missing" } }),
    );
}

#[test]
fn a_survey_answered_late_is_ignored() {
    let mut script = Script::new(host());
    let first = ticket_of(&script.check_agents(), "survey");
    let second = ticket_of(&script.check_agents(), "survey");
    let out = script.setup(first, found(&["npm-agent-2.0.0"]));
    assert_eq!(with(&out, "report")[0]["report"], "Debug");
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Unknown" })
    );
    script.setup(second, found(&[]));
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Missing" } }),
    );
    // A report on work CAF never asked for changes nothing.
    let before = script.state();
    let out = script.setup(4242, SetupEvent::Installed);
    assert_eq!(with(&out, "report")[0]["report"], "Debug");
    assert_eq!(script.state(), before);
}

#[test]
fn a_chat_waits_for_its_agent_to_be_installed_then_starts() {
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
    let chat = chat.expect("opened");
    assert!(with(&out, "start").is_empty(), "{out:#?}");
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" } }),
    );
    let (turn, out) = script.send(chat, "hello");
    assert!(with(&out, "start").is_empty());
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" }, "turns": [{ "id": turn.expect("sent").0, "life": { "type": "Waiting" } }] }),
    );
    let (done, out) = script.install("npm-agent");
    assert_eq!(done, Ok(()));
    let install = one(&out, "install");
    assert_eq!(
        install,
        json!({ "install": install["install"], "folder": "npm-agent-2.0.0", "how": "Npm", "private": null })
    );
    let install = install["install"].as_u64().expect("a ticket");
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Installing", "value": { "done": 0, "size": 40_000_000 } } }),
    );
    // Installing it again does nothing more.
    let (again, out) = script.install("npm-agent");
    assert_eq!(again, Ok(()));
    assert!(out.is_empty(), "{out:#?}");
    script.setup(install, SetupEvent::Progress { done: 1_000_000 });
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Installing", "value": { "done": 1_000_000 } } }),
    );
    let out = script.setup(install, SetupEvent::Installed);
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Installed", "value": { "version": "2.0.0" } })
    );
    // It starts from its install, which is held while it runs.
    let start = one(&out, "start");
    assert_eq!(
        start,
        json!({
            "start": start["start"],
            "program": "/caf/agents/npm-agent-2.0.0/node_modules/.bin/test-npm-acp",
            "hold": "/caf/agents/npm-agent-2.0.0",
            "command": {
                "program": "/caf/agents/npm-agent-2.0.0/node_modules/.bin/test-npm-acp",
                "args": ["--acp"],
                "env": ["TEST_NPM_MODE=acp"],
            },
        })
    );
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "Opening" } }),
    );
    let proc = start["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &npm_init());
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    let out = script.answers(proc, new, &json!({ "sessionId": "s1" }));
    sent(&out, "session/prompt");
    // The chat went from waiting to opening once, and then open.
    let states: Vec<Value> = (script.core.applied[&chat].steps.iter())
        .filter_map(|(change, _)| match &change.change {
            crate::change::Change::State(state) => serde_json::to_value(state).ok(),
            _ => None,
        })
        .map(|state| state["type"].clone())
        .collect();
    assert_eq!(states, ["Opening", "Open"]);
}

#[test]
fn a_failed_install_stops_the_messages_waiting_for_it() {
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let chat = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    let turn = script.send(chat, "hello").0.expect("sent");
    let install = ticket_of(&script.install("npm-agent").1, "install");
    let out = script.setup(install, SetupEvent::InstallFailed(InstallFailure::Offline));
    assert_eq!(with(&out, "report")[0]["report"], "Warn");
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Cannot", "value": { "type": "Failed", "value": { "type": "Offline" } } })
    );
    let whole = script.chat(chat);
    assert_has(
        &whole,
        &json!({ "state": { "type": "WaitingForAgent" }, "turns": [{ "id": turn.0, "life": { "type": "Stopped" } }, {}] }),
    );
    assert_eq!(notes(&whole), ["InstallFailed"]);
    // Installing again tries again, and the chat, still waiting, starts.
    let install = ticket_of(&script.install("npm-agent").1, "install");
    let out = script.setup(install, SetupEvent::Installed);
    one(&out, "start");
    // A failure the driver reports as its own is the install's.
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let install = ticket_of(&script.install("dl-agent").1, "install");
    script.setup(install, SetupEvent::Failed("tar: \u{1b}[31mbad\n".into()));
    assert_eq!(
        script.agent_status("dl-agent")["install"],
        json!({ "type": "Cannot", "value": { "type": "Failed", "value": { "type": "Other", "value": "tar: ^[[31mbad" } } })
    );
}

#[test]
fn an_install_makes_the_private_folder_its_recipe_uses() {
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let install = one(&script.install("dl-agent").1, "install");
    assert_eq!(
        install,
        json!({ "install": install["install"], "folder": "dl-agent-1.4.0", "how": "Download", "private": "dl-agent-private" })
    );
}

#[test]
fn stopping_an_install_stops_the_messages_waiting_for_it() {
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let chat = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    script.send(chat, "hello").0.expect("sent");
    let install = ticket_of(&script.install("npm-agent").1, "install");
    let (done, out) = script.stop_install("npm-agent");
    assert_eq!(done, Ok(()));
    assert_eq!(ticket_of(&out, "stop"), install);
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Missing", "value": { "size": 40_000_000 } })
    );
    assert_eq!(notes(&script.chat(chat)), ["NotInstalled"]);
    assert_eq!(
        script.stop_install("npm-agent").0,
        Err(Refused::NothingToStop)
    );
    // What it says as it stops changes nothing.
    let before = script.state();
    script.setup(install, SetupEvent::Progress { done: 5 });
    script.setup(install, SetupEvent::InstallFailed(InstallFailure::Stopped));
    assert_eq!(script.state()["setup"]["agents"], before["setup"]["agents"]);
    // Stopped too late, it's there: the chat, still waiting, starts.
    let install = ticket_of(&script.install("npm-agent").1, "install");
    script.stop_install("npm-agent").0.expect("stopped");
    let out = script.setup(install, SetupEvent::Installed);
    one(&out, "start");
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Installed" } }),
    );
}

#[test]
fn stopping_a_chat_waiting_for_its_agent_says_it_wasnt_installed() {
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let chat = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    let turn = script.send(chat, "hello").0.expect("sent");
    script.stop(chat).0.expect("stopped");
    let whole = script.chat(chat);
    assert_has(
        &whole,
        &json!({ "state": { "type": "Lost", "value": { "reason": { "type": "Stopped" } } }, "turns": [{ "id": turn.0, "life": { "type": "Stopped" } }, {}] }),
    );
    assert_eq!(notes(&whole), ["NotInstalled"]);
    // Sending again waits again.
    script.send(chat, "again").0.expect("sent");
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" } }),
    );
    // With nothing waiting, stopping says nothing.
    let other = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    script.stop(other).0.expect("stopped");
    assert!(notes(&script.chat(other)).is_empty());
}

#[test]
fn install_is_refused_where_caf_doesnt_install() {
    let mut script = Script::new(host());
    script.surveyed(&["npm-agent-2.0.0"]);
    assert_eq!(script.install("nobody").0, Err(Refused::NoSuchAgent));
    assert_eq!(script.install("claude").0, Err(Refused::NotInstallable));
    assert_eq!(
        script.install("npm-agent").0,
        Err(Refused::AlreadyInstalled)
    );
    assert_eq!(script.stop_install("nobody").0, Err(Refused::NoSuchAgent));
    assert_eq!(script.stop_install("claude").0, Err(Refused::NothingToStop));
}

#[test]
fn an_install_before_any_survey_surveys_too_and_starts_once_it_answers() {
    let mut script = Script::new(host());
    let chat = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" } }),
    );
    let out = script.install("npm-agent").1;
    let (survey, install) = (ticket_of(&out, "survey"), ticket_of(&out, "install"));
    // Installed before the survey answers, which looked too early to find
    // it: where it lives is known only once the survey answers.
    let out = script.setup(install, SetupEvent::Installed);
    assert!(with(&out, "start").is_empty());
    let out = script.setup(survey, found(&[]));
    one(&out, "start");
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Installed" } }),
    );
}

#[test]
fn signing_in_hands_on_a_link_to_one_of_the_recipes_sites() {
    let mut script = Script::new(host());
    script.surveyed(&["npm-agent-2.0.0"]);
    let (done, out) = script.sign_in("npm-agent");
    assert_eq!(done, Ok(()));
    let sign_in = one(&out, "sign_in");
    assert_eq!(
        sign_in,
        json!({
            "sign_in": sign_in["sign_in"],
            "command": {
                "program": "/caf/agents/npm-agent-2.0.0/node_modules/.bin/test-npm-acp",
                "args": ["login"],
                "env": ["TEST_NPM_MODE=acp"],
            },
            "link": { "type": "Browser" },
            "code_after": null,
            "paste": false,
        })
    );
    let ticket = sign_in["sign_in"].as_u64().expect("a ticket");
    assert_eq!(
        script.agent_status("npm-agent")["sign_in"],
        json!({ "type": "SigningIn", "value": { "url": null, "code": null, "paste": false } })
    );
    // Signing in again while it's under way does nothing more.
    let (again, out) = script.sign_in("npm-agent");
    assert_eq!((again, out.len()), (Ok(()), 0));
    let link = "https://Login.Npm.Example:443/auth?state=s3cret";
    script.setup(ticket, SetupEvent::Link(link.into()));
    assert_eq!(
        script.agent_status("npm-agent")["sign_in"]["value"]["url"],
        link
    );
    // It takes no paste.
    assert_eq!(
        script.paste("npm-agent", "code").0,
        Err(Refused::Unsupported)
    );
    script.setup(ticket, SetupEvent::Ended { ok: true });
    assert_eq!(
        script.agent_status("npm-agent")["sign_in"],
        signed_in("Unknown")
    );
    // Its adapter says how, once it runs.
    let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
    chat.expect("opened");
    let proc = one(&out, "start")["start"].as_u64().expect("a process");
    script.says(proc, &json!({ "jsonrpc": "2.0", "method": "_auth/status_update", "params": { "authStatus": { "kind": "api_key", "label": "Key" } } }));
    assert_eq!(
        script.agent_status("npm-agent")["sign_in"],
        signed_in("ApiKey")
    );
}

#[test]
fn a_link_to_any_other_site_stops_the_sign_in() {
    for link in [
        "https://login.npm.example@evil.example/auth?state=s3cret",
        "https://login.npm.example.evil.example/auth?state=s3cret",
        "https://evil.example/login.npm.example?state=s3cret",
        "http://login.npm.example/auth?state=s3cret",
        "https://login.npm.example/\nhttps://evil.example?state=s3cret",
    ] {
        let mut script = Script::new(host());
        script.surveyed(&["npm-agent-2.0.0"]);
        script.setup(1001, SetupEvent::Ended { ok: false });
        let ticket = ticket_of(&script.sign_in("npm-agent").1, "sign_in");
        let out = script.setup(ticket, SetupEvent::Link(link.into()));
        assert_eq!(ticket_of(&out, "stop"), ticket, "{link}");
        let reports = with(&out, "report");
        assert_eq!(reports[0]["report"], "Warn", "{link}");
        assert!(
            !reports[0]["message"].to_string().contains("s3cret"),
            "{link}"
        );
        assert_eq!(
            script.agent_status("npm-agent")["sign_in"],
            json!({ "type": "Unknown" }),
            "{link}"
        );
        // Whatever it says after, it isn't signed in by it.
        script.setup(
            ticket,
            SetupEvent::Link("https://login.npm.example/".into()),
        );
        script.setup(ticket, SetupEvent::Ended { ok: true });
        assert_eq!(
            script.agent_status("npm-agent")["sign_in"],
            json!({ "type": "Unknown" }),
            "{link}"
        );
        assert!(
            script.state()["setup"]["jobs"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
    }
}

#[test]
fn a_sign_in_that_prints_its_link_and_a_code_takes_a_paste() {
    let mut script = Script::new(host());
    let out = script.surveyed(&["dl-agent-1.4.0"]);
    script.setup(ticket_of(&out, "status"), SetupEvent::Status(None));
    let out = script.sign_in("dl-agent").1;
    let sign_in = one(&out, "sign_in");
    assert_eq!(
        sign_in,
        json!({
            "sign_in": sign_in["sign_in"],
            "command": {
                "program": "/caf/agents/dl-agent-1.4.0/bin/test-download",
                "args": ["auth", "login"],
                "env": ["TEST_HOME=/caf/agents/dl-agent-private"],
            },
            "link": { "type": "Output", "value": "Open this link: " },
            "code_after": "Your code: ",
            "paste": true,
        })
    );
    let ticket = sign_in["sign_in"].as_u64().expect("a ticket");
    script.setup(
        ticket,
        SetupEvent::Link("https://accounts.download.example/device".into()),
    );
    script.setup(ticket, SetupEvent::Code(" WXYZ-\u{7}1234 ".into()));
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        json!({ "type": "SigningIn", "value": { "url": "https://accounts.download.example/device", "code": "WXYZ-^G1234", "paste": true } })
    );
    let (done, out) = script.paste("dl-agent", "  abc#def \n");
    assert_eq!(done, Ok(()));
    assert_eq!(
        one(&out, "paste"),
        json!({ "paste": ticket, "text": "abc#def" })
    );
    // One line, of a sensible length.
    assert_eq!(
        script.paste("dl-agent", "abc\ndef").0,
        Err(Refused::Unsupported)
    );
    assert_eq!(
        script.paste("dl-agent", &"x".repeat(5000)).0,
        Err(Refused::Unsupported)
    );
    // Once it's done, the status command says how.
    let out = script.setup(ticket, SetupEvent::Ended { ok: true });
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        signed_in("Unknown")
    );
    script.setup(
        ticket_of(&out, "status"),
        SetupEvent::Status(Some(SignedInAs::ApiKey)),
    );
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        signed_in("ApiKey")
    );
    assert_eq!(
        script.paste("dl-agent", "late").0,
        Err(Refused::NotSigningIn)
    );
}

#[test]
fn a_sign_in_that_fails_or_is_stopped_leaves_things_as_they_were() {
    let mut script = Script::new(host());
    let out = script.surveyed(&["dl-agent-1.4.0"]);
    script.setup(ticket_of(&out, "status"), SetupEvent::Status(None));
    let first = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    // A report while it's under way is what it goes back to.
    script.check_agents();
    let status = ticket_of(&script.setup(1003, found(&["dl-agent-1.4.0"])), "status");
    script.setup(status, SetupEvent::Status(Some(SignedInAs::Account)));
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"]["type"],
        "SigningIn"
    );
    script.setup(first, SetupEvent::Ended { ok: false });
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        signed_in("Account")
    );
    let second = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    script.setup(second, SetupEvent::Failed("no such file".into()));
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        signed_in("Account")
    );
    let third = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    let (done, out) = script.stop_sign_in("dl-agent");
    assert_eq!(done, Ok(()));
    assert_eq!(ticket_of(&out, "stop"), third);
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        signed_in("Account")
    );
    assert_eq!(
        script.stop_sign_in("dl-agent").0,
        Err(Refused::NotSigningIn)
    );
    assert_eq!(script.paste("dl-agent", "x").0, Err(Refused::NotSigningIn));
    assert_eq!(script.stop_sign_in("nobody").0, Err(Refused::NoSuchAgent));
}

#[test]
fn caf_signs_in_only_with_a_recipe_it_can_run_now() {
    let mut script = Script::new(host());
    assert_eq!(script.sign_in("nobody").0, Err(Refused::NoSuchAgent));
    // The cautious profile has no recipe.
    assert_eq!(script.sign_in("plain").0, Err(Refused::CannotSignIn));
    assert_eq!(script.sign_out("plain").0, Err(Refused::CannotSignIn));
    // A file of an install that isn't there yet.
    script.surveyed(&[]);
    assert_eq!(script.sign_in("dl-agent").0, Err(Refused::CannotSignIn));
    // The host's own command, until a process of it has said it's the
    // adapter Claude's profile is for.
    assert_eq!(script.sign_in("claude").0, Err(Refused::CannotSignIn));
    let (chat, out) = script.open(OpenChat::new("claude", "/work"));
    chat.expect("opened");
    let proc = one(&out, "start")["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    script.answers(proc, init, &claude_init());
    let out = script.sign_in("claude").1;
    assert_eq!(
        one(&out, "sign_in")["command"],
        json!({ "program": "/opt/caf/claude-agent-acp", "args": ["--cli", "auth", "login", "--claudeai"], "env": [] })
    );
}

#[test]
fn a_host_command_that_isnt_its_profiles_adapter_is_never_run_to_sign_in() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("claude", "/work"));
    chat.expect("opened");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let mut other = claude_init();
    other["agentInfo"]["name"] = json!("someone-elses-acp");
    let out = script.answers(PROC, init, &other);
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.fails(
        PROC,
        new,
        &json!({ "code": -32000, "message": "Authentication required" }),
    );
    assert_eq!(script.sign_in("claude").0, Err(Refused::CannotSignIn));
    // So it isn't offered: the turn-less chat is just lost, and the agent
    // signed out.
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Lost", "value": { "reason": { "type": "SignInNeeded" } } } }),
    );
    assert_eq!(
        script.agent_status("claude")["sign_in"],
        json!({ "type": "SignedOut" })
    );
}

#[test]
fn a_chat_whose_turn_failed_for_sign_in_hears_when_the_user_signed_in() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script.fails(
        PROC,
        1002,
        &json!({ "code": -32000, "message": "Please run /login" }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "life": { "type": "Failed", "value": { "reason": { "type": "SignInNeeded" }, "remedy": { "type": "SignIn" } } } }] }),
    );
    assert_eq!(
        script.agent_status("claude")["sign_in"],
        json!({ "type": "SignedOut" })
    );
    // Another Claude chat, whose last turn didn't fail, hears nothing.
    let other = script
        .open(OpenChat::new("claude", "/elsewhere"))
        .0
        .expect("opened");
    let ticket = ticket_of(&script.sign_in("claude").1, "sign_in");
    script.setup(
        ticket,
        SetupEvent::Link("https://claude.com/cai/oauth/authorize?code=true".into()),
    );
    let out = script.setup(ticket, SetupEvent::Ended { ok: true });
    assert_eq!(notes(&script.chat(CHAT)), ["SignedIn"]);
    assert!(notes(&script.chat(other)).is_empty());
    // Nothing is sent again by itself: Send again is the user's.
    assert!(
        super::harness::writes(&out)
            .iter()
            .all(|(_, message)| message["method"] != "session/prompt")
    );
    // Its status command says how, run through the same adapter.
    let status = one(&out, "status");
    assert_eq!(
        status["command"],
        json!({ "program": "/opt/caf/claude-agent-acp", "args": ["--cli", "auth", "status", "--json"], "env": [] })
    );
    assert_eq!(
        status["how"],
        json!(["/authMethod", [
            ["claude.ai", { "type": "Account" }],
            ["oauth_token", { "type": "Account" }],
            ["api_key", { "type": "ApiKey" }],
            ["api_key_helper", { "type": "ApiKey" }],
            ["third_party", { "type": "Gateway" }],
        ]])
    );
    // Signing in again says it once.
    let ticket = ticket_of(&script.sign_in("claude").1, "sign_in");
    script.setup(ticket, SetupEvent::Ended { ok: true });
    assert_eq!(notes(&script.chat(CHAT)), ["SignedIn"]);
}

#[test]
fn an_agent_caf_cant_sign_in_to_keeps_send_again() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("plain", "/work"));
    let chat = chat.expect("opened");
    let turn = script.send(chat, "hi").0.expect("sent");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(PROC, init, &super::plain_init(false));
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.fails(
        PROC,
        new,
        &json!({ "code": -32000, "message": "Authentication required" }),
    );
    assert_has(
        &script.chat(chat),
        &json!({ "turns": [{ "id": turn.0, "life": { "type": "Failed", "value": { "reason": { "type": "SignInNeeded" }, "remedy": { "type": "SendAgain" } } } }] }),
    );
    assert_eq!(
        script.agent_status("plain")["sign_in"],
        json!({ "type": "SignedOut" })
    );
}

#[test]
fn messages_wait_while_their_agent_is_signed_in_to() {
    let mut script = Script::new(host());
    script.surveyed(&["npm-agent-2.0.0"]);
    let ticket = ticket_of(&script.sign_in("npm-agent").1, "sign_in");
    let chat = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    script.send(chat, "hello").0.expect("sent");
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" } }),
    );
    // No note: it's installed.
    let out = script.setup(ticket, SetupEvent::Ended { ok: true });
    one(&out, "start");
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "Opening" } }),
    );
}

#[test]
fn signing_out_runs_the_recipes_command() {
    let mut script = Script::new(host());
    let out = script.surveyed(&["dl-agent-1.4.0"]);
    script.setup(
        ticket_of(&out, "status"),
        SetupEvent::Status(Some(SignedInAs::Account)),
    );
    let (done, out) = script.sign_out("dl-agent");
    assert_eq!(done, Ok(()));
    let run = one(&out, "run");
    assert_eq!(
        run["command"],
        json!({ "program": "/caf/agents/dl-agent-1.4.0/bin/test-download", "args": ["auth", "logout"], "env": [] })
    );
    let run = run["run"].as_u64().expect("a ticket");
    script.setup(run, SetupEvent::Ended { ok: false });
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        signed_in("Account")
    );
    let run = ticket_of(&script.sign_out("dl-agent").1, "run");
    script.setup(run, SetupEvent::Ended { ok: true });
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        json!({ "type": "SignedOut" })
    );
    // Signing out while signing in stops the sign-in first.
    let sign_in = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    let out = script.sign_out("dl-agent").1;
    assert_eq!(ticket_of(&out, "stop"), sign_in);
    one(&out, "run");
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        json!({ "type": "SignedOut" })
    );
}

#[test]
fn the_adapters_own_reports_are_read_for_how_its_signed_in_and_nothing_else() {
    let mut script = Script::with_claude();
    let push = |kind: &str| {
        json!({ "jsonrpc": "2.0", "method": "_auth/status_update", "params": { "authStatus": {
        "kind": kind, "label": "Claude Max", "account": { "email": "me@example.com", "organization": "Me's Org", "plan": "max" }
    } } })
    };
    for (kind, state) in [
        ("account", signed_in("Account")),
        ("api_key", signed_in("ApiKey")),
        ("gateway", signed_in("Gateway")),
        ("external", signed_in("Gateway")),
        ("none", json!({ "type": "SignedOut" })),
    ] {
        let out = script.says(PROC, &push(kind));
        assert!(!format!("{out:?}").contains("example.com"));
        assert_eq!(script.agent_status("claude")["sign_in"], state, "{kind}");
    }
    let out = script.says(PROC, &push("telepathy"));
    assert_eq!(with(&out, "report")[0]["report"], "Debug");
    assert!(!format!("{out:?}").contains("telepathy"));
    assert_eq!(
        script.agent_status("claude")["sign_in"],
        json!({ "type": "SignedOut" })
    );
    let state = script.state().to_string();
    assert!(!state.contains("example.com") && !state.contains("Me's Org"));
    // An agent whose profile doesn't say it reports so isn't read.
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("plain", "/work"));
    chat.expect("opened");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    script.answers(PROC, init, &super::plain_init(false));
    script.says(PROC, &push("account"));
    assert_eq!(
        script.agent_status("plain")["sign_in"],
        json!({ "type": "Unknown" })
    );
}

#[test]
fn old_versions_are_removed_once_the_pinned_one_has_started() {
    let mut per_chat = host();
    per_chat.grouping = crate::host::Grouping::PerChat;
    let mut script = Script::new(per_chat);
    script.surveyed(&[
        "npm-agent-1.9.0",
        "npm-agent-2.0.0",
        "npm-agent-2-1.0.0",
        "npm-agent-private",
        "dl-agent-1.3.0",
    ]);
    let started = |script: &mut Script| {
        let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
        chat.expect("opened");
        let proc = one(&out, "start")["start"].as_u64().expect("a process");
        let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
        script.answers(proc, init, &npm_init())
    };
    // Only another version of this agent goes: never `npm-agent-2`'s.
    let out = started(&mut script);
    let remove = one(&out, "remove");
    assert_eq!(remove["folder"], "npm-agent-1.9.0");
    // In use by another program: kept, and tried again next time.
    script.setup(
        remove["remove"].as_u64().expect("a ticket"),
        SetupEvent::Removed { removed: false },
    );
    let out = started(&mut script);
    let remove = one(&out, "remove");
    assert_eq!(remove["folder"], "npm-agent-1.9.0");
    // Not asked twice while it's being removed.
    assert!(with(&started(&mut script), "remove").is_empty());
    script.setup(
        remove["remove"].as_u64().expect("a ticket"),
        SetupEvent::Removed { removed: true },
    );
    assert_eq!(
        script.state()["setup"]["machine"]["installed"],
        json!([
            "dl-agent-1.3.0",
            "npm-agent-2-1.0.0",
            "npm-agent-2.0.0",
            "npm-agent-private"
        ])
    );
    assert!(with(&started(&mut script), "remove").is_empty());
}

#[test]
fn shutting_down_stops_installs_and_sign_ins() {
    let mut script = Script::new(host());
    script.surveyed(&["dl-agent-1.4.0"]);
    let install = ticket_of(&script.install("npm-agent").1, "install");
    let sign_in = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    let out = script.shutdown();
    let stops: Vec<u64> = (with(&out, "stop").iter())
        .filter_map(|each| each["stop"].as_u64())
        .collect();
    assert_eq!(stops, [install, sign_in]);
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Missing" } }),
    );
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        json!({ "type": "Unknown" })
    );
}

impl Script {
    /// Checks each agent's `can_sign_in` says what [`Core::sign_in`]
    /// would answer now, asking a copy of the core, so nothing here
    /// changes. Returns each agent's, in order.
    ///
    /// [`Core::sign_in`]: crate::Core::sign_in
    #[track_caller]
    fn can_sign_in(&self) -> Vec<bool> {
        let statuses = self.core.agents();
        for status in &statuses {
            let mut copy = self.copy();
            let accepted = copy.sign_in(self.now(), &status.name, &mut Vec::new());
            assert_eq!(
                status.can_sign_in,
                accepted.is_ok(),
                "{}: can_sign_in says {}, and sign_in answers {accepted:?}",
                status.name.0,
                status.can_sign_in
            );
        }
        statuses.iter().map(|status| status.can_sign_in).collect()
    }
}

#[test]
fn can_sign_in_is_what_sign_in_would_answer_at_every_step() {
    // claude (the host's command), plain (no recipe), npm-agent and
    // dl-agent (CAF installs them).
    let mut script = Script::new(host());
    // Nothing known: Claude's adapter hasn't said it's Claude's, and
    // neither installed agent is there.
    assert_eq!(script.can_sign_in(), [false, false, false, false]);
    // Claude, once its adapter has answered as Claude's.
    script.open_claude();
    assert_eq!(script.can_sign_in(), [true, false, false, false]);
    // The survey runs both status commands, Claude's and dl-agent's.
    let out = script.surveyed(&["dl-agent-1.4.0"]);
    assert_eq!(script.can_sign_in(), [true, false, false, true]);
    for status in with(&out, "status") {
        let ticket = status["status"].as_u64().expect("a ticket");
        script.setup(ticket, SetupEvent::Status(None));
    }
    assert_eq!(script.can_sign_in(), [true, false, false, true]);

    // Installing: only once it's there.
    let install = ticket_of(&script.install("npm-agent").1, "install");
    script.setup(install, SetupEvent::Progress { done: 1000 });
    assert_eq!(script.can_sign_in(), [true, false, false, true]);
    script.setup(install, SetupEvent::Installed);
    assert_eq!(script.can_sign_in(), [true, false, true, true]);

    // A sign-in under way: sign_in accepts it as it stands.
    let ticket = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    assert_eq!(script.can_sign_in(), [true, false, true, true]);
    script.setup(
        ticket,
        SetupEvent::Link("https://accounts.download.example/device".into()),
    );
    assert_eq!(script.can_sign_in(), [true, false, true, true]);
    script.stop_sign_in("dl-agent").0.expect("stopped");
    assert_eq!(script.can_sign_in(), [true, false, true, true]);
    let ticket = ticket_of(&script.sign_in("npm-agent").1, "sign_in");
    script.setup(ticket, SetupEvent::Ended { ok: true });
    assert_eq!(script.can_sign_in(), [true, false, true, true]);
    script.sign_out("npm-agent").0.expect("signing out");
    assert_eq!(script.can_sign_in(), [true, false, true, true]);

    // Shutting down.
    script.shutdown();
    script.can_sign_in();
}

#[test]
fn can_sign_in_stays_false_for_a_host_command_that_isnt_its_profiles_adapter() {
    let mut script = Script::new(host());
    let (_, out) = script.open(OpenChat::new("claude", "/work"));
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let mut other = claude_init();
    other["agentInfo"]["name"] = json!("someone-elses-acp");
    script.answers(PROC, init, &other);
    assert!(!script.can_sign_in()[0]);
}

#[test]
fn can_sign_in_is_false_where_the_machine_cant_install() {
    let mut script = Script::new(host());
    let out = script.check_agents();
    script.setup(
        ticket_of(&out, "survey"),
        found_on(&[], None, false, "riscv64"),
    );
    assert_eq!(script.can_sign_in(), [false, false, false, false]);
}

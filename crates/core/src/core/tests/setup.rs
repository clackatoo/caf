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
        used: Vec::new(),
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
            { "name": "claude", "install": { "type": "Host" }, "sign_in": { "type": "Unknown" } },
            { "name": "plain", "install": { "type": "Host" }, "sign_in": { "type": "Unknown" } },
            { "name": "npm-agent", "install": { "type": "Unknown" }, "sign_in": { "type": "Unknown" } },
            { "name": "dl-agent", "install": { "type": "Unknown" }, "sign_in": { "type": "Unknown" } },
            { "name": "npm-twin", "install": { "type": "Unknown" }, "sign_in": { "type": "Unknown" } },
            { "name": "claude-npx", "install": { "type": "Host" }, "sign_in": { "type": "Unknown" } },
        ])
    );
    let out = script.surveyed(&["test-download-1.4.0", "someone-else-1.0.0"]);
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
                "program": "/caf/agents/test-download-1.4.0/bin/test-download",
                "args": ["auth", "status"],
                "env": ["TEST_HOME=/caf/agents/test-download-private"],
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
    // A status command whose agent is signed out says so.
    let out = script.check_agents();
    let survey = ticket_of(&out, "survey");
    let out = script.setup(survey, found(&["test-download-1.4.0"]));
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
    let out = script.setup(first, found(&["test-npm-2.0.0"]));
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
        json!({ "install": install["install"], "folder": "test-npm-2.0.0", "how": "Npm" })
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
            "program": "/caf/agents/test-npm-2.0.0/node_modules/.bin/test-npm-acp",
            "hold": "/caf/agents/test-npm-2.0.0",
            "command": {
                "program": "/caf/agents/test-npm-2.0.0/node_modules/.bin/test-npm-acp",
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
    script.surveyed(&["test-npm-2.0.0"]);
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
    script.surveyed(&["test-npm-2.0.0"]);
    let (done, out) = script.sign_in("npm-agent");
    assert_eq!(done, Ok(()));
    let sign_in = one(&out, "sign_in");
    assert_eq!(
        sign_in,
        json!({
            "sign_in": sign_in["sign_in"],
            "command": {
                "program": "/caf/agents/test-npm-2.0.0/node_modules/.bin/test-npm-acp",
                "args": ["login"],
                "env": ["TEST_NPM_MODE=acp"],
            },
            "link": { "type": "Browser" },
            "code_after": null,
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
        script.surveyed(&["test-npm-2.0.0"]);
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
    let out = script.surveyed(&["test-download-1.4.0"]);
    script.setup(ticket_of(&out, "status"), SetupEvent::Status(None));
    let out = script.sign_in("dl-agent").1;
    let sign_in = one(&out, "sign_in");
    assert_eq!(
        sign_in,
        json!({
            "sign_in": sign_in["sign_in"],
            "command": {
                "program": "/caf/agents/test-download-1.4.0/bin/test-download",
                "args": ["auth", "login"],
                "env": ["TEST_HOME=/caf/agents/test-download-private"],
            },
            "link": { "type": "Output", "value": "Open this link: " },
            "code_after": "Your code: ",
        })
    );
    let ticket = sign_in["sign_in"].as_u64().expect("a ticket");
    script.setup(
        ticket,
        SetupEvent::Link("https://accounts.download.example/device".into()),
    );
    // A code that can't be shown as it came is dropped, never changed:
    // cut or cleaned, it would be the wrong code.
    for bad in ["WXYZ-\u{7}1234".to_owned(), "W".repeat(65)] {
        let out = script.setup(ticket, SetupEvent::Code(bad));
        assert_eq!(with(&out, "report")[0]["report"], "Warn");
        assert_eq!(
            script.agent_status("dl-agent")["sign_in"]["value"]["code"],
            Value::Null
        );
    }
    script.setup(ticket, SetupEvent::Code(" WXYZ-1234 ".into()));
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        json!({ "type": "SigningIn", "value": { "url": "https://accounts.download.example/device", "code": "WXYZ-1234", "paste": true } })
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
    // Nothing at all isn't a paste.
    assert_eq!(script.paste("dl-agent", "  ").0, Err(Refused::Unsupported));
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
    let out = script.surveyed(&["test-download-1.4.0"]);
    script.setup(ticket_of(&out, "status"), SetupEvent::Status(None));
    let first = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    // A report while it's under way is what it goes back to.
    script.check_agents();
    let status = ticket_of(
        &script.setup(1003, found(&["test-download-1.4.0"])),
        "status",
    );
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
    let out = script.setup(
        second,
        SetupEvent::Failed("printed https://accounts.download.example/?code=s3cret".into()),
    );
    // What the driver said is never logged: it might be the command's.
    assert!(!format!("{out:?}").contains("s3cret"));
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
fn messages_wait_while_their_signed_out_agent_is_signed_in_to() {
    let mut per_chat = host();
    per_chat.grouping = crate::host::Grouping::PerChat;
    let mut script = Script::new(per_chat);
    script.surveyed(&["test-npm-2.0.0"]);
    // Signed out, as its adapter said.
    let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
    let first = chat.expect("opened");
    let proc = one(&out, "start")["start"].as_u64().expect("a process");
    script.says(proc, &json!({ "jsonrpc": "2.0", "method": "_auth/status_update", "params": { "authStatus": { "kind": "none" } } }));
    script.close(first).0.expect("closed");
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
    let out = script.setup(ticket, SetupEvent::Ended { ok: true });
    one(&out, "start");
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "Opening" } }),
    );
    // Stopping a chat waiting on a sign-in says nothing of installing.
    let ticket = ticket_of(&script.sign_in("npm-agent").1, "sign_in");
    script.setup(ticket, SetupEvent::Ended { ok: false });
    let mut script = Script::new(host());
    let out = script.surveyed(&["test-download-1.4.0"]);
    script.setup(ticket_of(&out, "status"), SetupEvent::Status(None));
    script.sign_in("dl-agent").0.expect("signing in");
    let chat = script
        .open(OpenChat::new("dl-agent", "/work"))
        .0
        .expect("opened");
    script.send(chat, "hello").0.expect("sent");
    script.stop(chat).0.expect("stopped");
    assert!(notes(&script.chat(chat)).is_empty());
}

#[test]
fn a_sign_in_to_an_agent_not_known_to_be_signed_out_holds_no_chat() {
    let mut script = Script::new(host());
    script.surveyed(&["test-npm-2.0.0"]);
    script.sign_in("npm-agent").0.expect("signing in");
    let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
    chat.expect("opened");
    one(&out, "start");
}

#[test]
fn a_sign_in_nobody_finishes_is_stopped_and_the_chats_waiting_on_it_start() {
    let mut script = Script::new(host());
    let out = script.surveyed(&["test-download-1.4.0"]);
    script.setup(ticket_of(&out, "status"), SetupEvent::Status(None));
    let ticket = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    let chat = script
        .open(OpenChat::new("dl-agent", "/work"))
        .0
        .expect("opened");
    script.send(chat, "hello").0.expect("sent");
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" } }),
    );
    // Its agent's status says why.
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"]["type"],
        "SigningIn"
    );
    script.at(10 * 60 * 1000 - 1);
    assert!(with(&script.woken(), "stop").is_empty());
    script.at(10 * 60 * 1000);
    let out = script.woken();
    assert_eq!(ticket_of(&out, "stop"), ticket);
    assert!(
        with(&out, "report")
            .iter()
            .any(|each| each["report"] == "Warn")
    );
    one(&out, "start");
    assert_eq!(
        script.agent_status("dl-agent")["sign_in"],
        json!({ "type": "SignedOut" })
    );
    // Its time is the host's to set.
    let mut quick = host();
    quick.timers.sign_in = std::time::Duration::from_secs(1);
    let mut script = Script::new(quick);
    script.surveyed(&["test-npm-2.0.0"]);
    let ticket = ticket_of(&script.sign_in("npm-agent").1, "sign_in");
    script.at(1000);
    assert_eq!(ticket_of(&script.woken(), "stop"), ticket);
    // Once ended, nothing more is due.
    script.at(5000);
    assert!(with(&script.woken(), "stop").is_empty());
}

#[test]
fn signing_out_runs_the_recipes_command() {
    let mut script = Script::new(host());
    let out = script.surveyed(&["test-download-1.4.0"]);
    script.setup(
        ticket_of(&out, "status"),
        SetupEvent::Status(Some(SignedInAs::Account)),
    );
    let (done, out) = script.sign_out("dl-agent");
    assert_eq!(done, Ok(()));
    let run = one(&out, "run");
    assert_eq!(
        run["command"],
        json!({ "program": "/caf/agents/test-download-1.4.0/bin/test-download", "args": ["auth", "logout"], "env": [] })
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

/// A day, in milliseconds.
const DAY: i64 = 24 * 60 * 60 * 1000;

/// The wall clock when scripts start, as the harness sets it.
const EPOCH: i64 = 1_800_000_000_000;

#[test]
fn only_older_versions_unused_for_14_days_are_removed_once_the_pinned_one_started() {
    let mut per_chat = host();
    per_chat.grouping = crate::host::Grouping::PerChat;
    let mut script = Script::new(per_chat);
    let folders = [
        "test-npm-1.8.0",
        "test-npm-1.9.0",
        "test-npm-2.0.0-rc.1",
        "test-npm-2.0.0",
        "test-npm-2.1.0",
        "test-npm-2-1.0.0",
        "test-npm-1.0.0-x/../../elsewhere",
        "test-npm-private",
        "test-download-1.3.0",
    ];
    let used: Vec<(String, crate::time::Wall)> = [
        ("test-npm-1.8.0", EPOCH - 30 * DAY),
        // Another program still uses it now and then.
        ("test-npm-1.9.0", EPOCH - 13 * DAY),
        ("test-npm-2.0.0-rc.1", EPOCH - 14 * DAY),
        // Another program's newer pin: never removed by this one.
        ("test-npm-2.1.0", EPOCH - 400 * DAY),
        ("test-npm-2-1.0.0", EPOCH - 400 * DAY),
        ("test-npm-1.0.0-x/../../elsewhere", EPOCH - 400 * DAY),
        ("test-download-1.3.0", EPOCH - 400 * DAY),
    ]
    .into_iter()
    .map(|(folder, at)| (folder.to_owned(), crate::time::Wall(at)))
    .collect();
    // `test-npm-1.0.0`'s use isn't known, so it's never removed.
    let out = script.check_agents();
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
    let mut folders: Vec<String> = folders.iter().map(|&each| each.to_owned()).collect();
    folders.push("test-npm-1.0.0".to_owned());
    script.setup(
        ticket_of(&out, "survey"),
        SetupEvent::Surveyed {
            root,
            installed: folders,
            node,
            npm,
            platform,
            used,
        },
    );
    let started = |script: &mut Script, info: Value| {
        let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
        chat.expect("opened");
        let proc = one(&out, "start")["start"].as_u64().expect("a process");
        let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
        script.answers(proc, init, &info)
    };
    // An adapter in the pinned folder that isn't the version checked
    // removes nothing.
    let mut wrong = npm_init();
    wrong["agentInfo"]["version"] = json!("2.0.1");
    assert!(with(&started(&mut script, wrong), "remove").is_empty());
    let out = started(&mut script, npm_init());
    let mut removed: Vec<Value> = with(&out, "remove")
        .into_iter()
        .map(|each| each["folder"].clone())
        .collect();
    removed.sort_by_key(ToString::to_string);
    assert_eq!(removed, ["test-npm-1.8.0", "test-npm-2.0.0-rc.1"]);
    let tickets: Vec<u64> = (with(&out, "remove").iter())
        .filter_map(|each| each["remove"].as_u64())
        .collect();
    // In use by another program: kept, and tried again next time.
    script.setup(tickets[0], SetupEvent::Removed { removed: false });
    // Not asked twice while it's being removed.
    let out = started(&mut script, npm_init());
    assert_eq!(with(&out, "remove").len(), 1);
    script.setup(tickets[1], SetupEvent::Removed { removed: true });
    let again = ticket_of(&out, "remove");
    script.setup(again, SetupEvent::Removed { removed: true });
    assert!(with(&started(&mut script, npm_init()), "remove").is_empty());
    let installed = script.state()["setup"]["machine"]["installed"].clone();
    assert!(!installed.to_string().contains("1.8.0") && !installed.to_string().contains("rc.1"));
    assert!(installed.to_string().contains("test-npm-1.9.0"));
}

#[test]
fn shutting_down_stops_installs_and_sign_ins() {
    let mut script = Script::new(host());
    script.surveyed(&["test-download-1.4.0"]);
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

#[test]
fn a_host_commands_own_arguments_are_kept_when_its_adapter_signs_in() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("claude-npx", "/work"));
    chat.expect("opened");
    let proc = one(&out, "start")["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    script.answers(proc, init, &claude_init());
    // Never `npx --cli auth login`, which would fetch and run a package
    // called `auth`.
    let npx = |args: &[&str]| {
        let mut all = vec!["-y", "@agentclientprotocol/claude-agent-acp@0.98.0"];
        all.extend(args);
        json!({ "program": "/usr/bin/npx", "args": all, "env": ["NPM_CONFIG_CACHE=/tmp/npm"] })
    };
    let out = script.sign_in("claude-npx").1;
    assert_eq!(
        one(&out, "sign_in")["command"],
        npx(&["--cli", "auth", "login", "--claudeai"])
    );
    let sign_in = ticket_of(&out, "sign_in");
    let out = script.setup(sign_in, SetupEvent::Ended { ok: true });
    assert_eq!(
        one(&out, "status")["command"],
        npx(&["--cli", "auth", "status", "--json"])
    );
    let out = script.sign_out("claude-npx").1;
    assert_eq!(
        one(&out, "run")["command"],
        npx(&["--cli", "auth", "logout"])
    );
}

#[test]
fn agents_of_one_profile_share_one_install_whatever_the_host_calls_them() {
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let out = script.install("npm-agent").1;
    assert_eq!(one(&out, "install")["folder"], "test-npm-2.0.0");
    let install = ticket_of(&out, "install");
    assert_has(
        &script.agent_status("npm-twin"),
        &json!({ "install": { "type": "Installing" } }),
    );
    // Installing it by its other name waits on the same install.
    let (done, out) = script.install("npm-twin");
    assert_eq!((done, out.len()), (Ok(()), 0));
    script.setup(install, SetupEvent::Progress { done: 7 });
    assert_has(
        &script.agent_status("npm-twin"),
        &json!({ "install": { "type": "Installing", "value": { "done": 7 } } }),
    );
    script.setup(install, SetupEvent::Installed);
    assert_has(
        &script.agent_status("npm-twin"),
        &json!({ "install": { "type": "Installed" } }),
    );
    assert_eq!(script.install("npm-twin").0, Err(Refused::AlreadyInstalled));
    let (chat, out) = script.open(OpenChat::new("npm-twin", "/work"));
    chat.expect("opened");
    assert_eq!(one(&out, "start")["hold"], "/caf/agents/test-npm-2.0.0");
    // Stopping it by either name stops it for both.
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let install = ticket_of(&script.install("npm-agent").1, "install");
    assert_eq!(
        ticket_of(&script.stop_install("npm-twin").1, "stop"),
        install
    );
    for agent in ["npm-agent", "npm-twin"] {
        assert_has(
            &script.agent_status(agent),
            &json!({ "install": { "type": "Missing" } }),
        );
    }
}

#[test]
fn an_install_finishing_after_it_was_stopped_ends_any_other_under_way() {
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let chat = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    script.send(chat, "hello").0.expect("sent");
    let first = ticket_of(&script.install("npm-agent").1, "install");
    script.stop_install("npm-agent").0.expect("stopped");
    let second = ticket_of(&script.install("npm-agent").1, "install");
    let out = script.setup(first, SetupEvent::Installed);
    assert_eq!(ticket_of(&out, "stop"), second);
    one(&out, "start");
    assert_eq!(
        script.stop_install("npm-agent").0,
        Err(Refused::NothingToStop)
    );
    script.setup(second, SetupEvent::InstallFailed(InstallFailure::Stopped));
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Installed" } }),
    );
    // A stopped install's progress, or failure in its own words, changes
    // nothing of a later one.
    let mut script = Script::new(host());
    script.surveyed(&[]);
    let first = ticket_of(&script.install("npm-agent").1, "install");
    script.stop_install("npm-agent").0.expect("stopped");
    script.setup(first, SetupEvent::Failed("killed".into()));
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Missing" } }),
    );
    let first = ticket_of(&script.install("npm-agent").1, "install");
    script.stop_install("npm-agent").0.expect("stopped");
    script.install("npm-agent").0.expect("installing");
    script.setup(first, SetupEvent::Progress { done: 99 });
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Installing", "value": { "done": 0 } } }),
    );
}

#[test]
fn a_survey_that_fails_is_said_and_can_be_asked_again() {
    let mut script = Script::new(host());
    let chat = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    let out = script.install("npm-agent").1;
    let (survey, install) = (ticket_of(&out, "survey"), ticket_of(&out, "install"));
    let out = script.setup(survey, SetupEvent::Failed("HOME isn't set".into()));
    assert_eq!(with(&out, "report")[0]["report"], "Warn");
    assert_eq!(script.state()["setup"]["survey"], Value::Null);
    script.setup(install, SetupEvent::Installed);
    // Where installs live isn't known yet, so it still waits.
    assert_has(
        &script.chat(chat),
        &json!({ "state": { "type": "WaitingForAgent" } }),
    );
    let survey = ticket_of(&script.check_agents(), "survey");
    one(&script.setup(survey, found(&["test-npm-2.0.0"])), "start");
}

#[test]
fn a_survey_keeps_what_it_cant_have_seen_and_nothing_else() {
    let mut script = Script::new(host());
    script.surveyed(&[]);
    // Still installing: a survey changes nothing, and nothing is installed
    // twice.
    let install = ticket_of(&script.install("npm-agent").1, "install");
    script.surveyed(&[]);
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Installing" } }),
    );
    assert!(script.install("npm-agent").1.is_empty());
    // Installed with no survey out: a later survey that doesn't find it
    // is right, as the folder was removed since.
    script.setup(install, SetupEvent::Installed);
    script.surveyed(&[]);
    assert_has(
        &script.agent_status("npm-agent"),
        &json!({ "install": { "type": "Missing" } }),
    );
    // Why the last try failed outlasts a survey.
    let install = ticket_of(&script.install("npm-agent").1, "install");
    script.setup(install, SetupEvent::InstallFailed(InstallFailure::NoSpace));
    script.surveyed(&[]);
    assert_eq!(
        script.agent_status("npm-agent")["install"],
        json!({ "type": "Cannot", "value": { "type": "Failed", "value": { "type": "NoSpace" } } })
    );
}

#[test]
fn an_agent_caf_has_no_recipe_for_says_so() {
    use crate::agent::{AgentSpec, Launch};
    use crate::profiles::ProfileChoice;
    let mut spec = AgentSpec::installed("odd", crate::profiles::Builtin::TestNpm);
    spec.launch = Launch::Installed;
    spec.profile = ProfileChoice::Cautious;
    let mut core = crate::Core::<crate::NoAppItems>::new(host(), vec![spec]);
    let now = |ms: i64| crate::time::Now {
        wall: crate::time::Wall(ms),
        mono: crate::time::Mono(0),
    };
    let mut out = Vec::new();
    core.check_agents(now(1), &mut out);
    let Some(crate::Directive::Setup(crate::directive::SetupDirective::Survey { ticket })) =
        out.first()
    else {
        panic!("{out:?}")
    };
    let event = crate::Event::Setup {
        ticket: *ticket,
        event: found(&[]),
    };
    core.handle(now(2), event, &mut Vec::new());
    assert_eq!(
        serde_json::to_value(&core.agents()[0].install).expect("JSON"),
        json!({ "type": "Cannot", "value": { "type": "NoRecipe" } })
    );
}

#[test]
fn stopping_a_chat_before_anything_is_known_says_nothing_of_installing() {
    let mut script = Script::new(host());
    let chat = script
        .open(OpenChat::new("npm-agent", "/work"))
        .0
        .expect("opened");
    script.send(chat, "hi").0.expect("sent");
    script.stop(chat).0.expect("stopped");
    assert!(notes(&script.chat(chat)).is_empty());
}

#[test]
fn stopping_an_install_leaves_an_open_chats_messages_alone() {
    let mut script = Script::new(host());
    script.surveyed(&["test-npm-2.0.0"]);
    let (chat, out) = script.open(OpenChat::new("npm-agent", "/work"));
    let chat = chat.expect("opened");
    let proc = one(&out, "start")["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &npm_init());
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.answers(proc, new, &json!({ "sessionId": "s1" }));
    script.send(chat, "first").0.expect("sent");
    let queued = script.send(chat, "second").0.expect("sent");
    // The folder went from under it, and the user installs it again.
    script.surveyed(&[]);
    script.install("npm-agent").0.expect("installing");
    script.stop_install("npm-agent").0.expect("stopped");
    let whole = script.chat(chat);
    let turn = (whole["turns"].as_array().into_iter().flatten())
        .find(|turn| turn["id"] == queued.0)
        .cloned()
        .unwrap_or_default();
    assert_eq!(turn["life"], json!({ "type": "Waiting" }));
    assert!(notes(&whole).is_empty());
}

#[test]
fn a_status_command_runs_once_at_a_time() {
    let mut script = Script::new(host());
    let out = script.surveyed(&["test-download-1.4.0"]);
    one(&out, "status");
    let out = script.surveyed(&["test-download-1.4.0"]);
    assert!(with(&out, "status").is_empty(), "{out:#?}");
}

#[test]
fn a_link_too_long_to_keep_stops_the_sign_in() {
    let mut script = Script::new(host());
    script.surveyed(&["test-npm-2.0.0"]);
    let ticket = ticket_of(&script.sign_in("npm-agent").1, "sign_in");
    let link = format!("https://login.npm.example/?state={}", "x".repeat(4096));
    let out = script.setup(ticket, SetupEvent::Link(link));
    assert_eq!(ticket_of(&out, "stop"), ticket);
}

#[test]
fn a_closed_chat_hears_nothing_of_a_sign_in() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script.fails(
        PROC,
        1002,
        &json!({ "code": -32000, "message": "Please run /login" }),
    );
    script.close(CHAT).0.expect("closed");
    let ticket = ticket_of(&script.sign_in("claude").1, "sign_in");
    script.setup(ticket, SetupEvent::Ended { ok: true });
    assert!(notes(&script.chat(CHAT)).is_empty());
}

#[test]
fn a_recorded_paste_without_its_text_replays() {
    let mut script = Script::new(host());
    let out = script.surveyed(&["test-download-1.4.0"]);
    script.setup(ticket_of(&out, "status"), SetupEvent::Status(None));
    let ticket = ticket_of(&script.sign_in("dl-agent").1, "sign_in");
    let recorded =
        r#"{"type":"Host","value":{"type":"Paste","value":{"agent":"dl-agent","text":null}}}"#;
    let event: crate::Event = serde_json::from_str(recorded).expect("it reads");
    let out = script.event(event);
    assert_eq!(one(&out, "paste"), json!({ "paste": ticket, "text": "" }));
}

//! CAF's own tools, end to end: the fake adapter starts the real relay
//! and calls the real Read, Edit, Write, Bash and TaskStop, which CAF's
//! own runner carries out on the test's own folder.

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use caf_core::OpenChat;
use caf_core::agent::SpawnCommand;
use caf_core::chat::{Chat, ItemKind, UserMessage};
use caf_core::directive::PermissionRequest;
use caf_core::event::{Exit, PolicyAnswer};
use caf_core::ids::{ChatId, JobId};
use caf_core::tool::{CafTool, Ending, Piece, StoppedBy, ToolCall, ToolStatus, Tools};
use caf_driver_tokio::{Caf, ChatHandle, Policy, Relay, Running};
use common::{Dir, PATIENCE, after_handshake, end_turn, fake, host, prompt, until};
use serde_json::{Value, json};

/// Allows every call.
struct AllowAll;

impl Policy for AllowAll {
    fn decide(
        &mut self,
        _: ChatId,
        _: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        Box::pin(std::future::ready(PolicyAnswer::Allow))
    }
}

/// What Claude Code adds to the answer to a change.
const CURRENT: &str = " (file state is current in your context \u{2014} no need to Read it back)";

/// A test's CAF, serving its own tools on the test's folder.
struct Own {
    dir: Dir,
    /// The test's files: `dir`, as the shell names it.
    work: PathBuf,
    caf: Caf,
    running: Option<Running>,
}

/// CAF serving its own tools through the fake's program, which runs
/// `steps` (after the handshake, the relay and the first prompt) as the
/// agent: `steps` is given the folder its files are in.
fn serve(steps: impl FnOnce(&Path) -> Vec<Value>) -> Own {
    serve_with(steps, |_| {})
}

/// [`serve`], with `config` changed as `change` says first.
fn serve_with(
    steps: impl FnOnce(&Path) -> Vec<Value>,
    change: impl FnOnce(&mut caf_driver_tokio::Config),
) -> Own {
    let dir = Dir::new();
    let work = dir.path("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(work).unwrap();
    let mut all = vec![
        json!({"mcp_start": {"discover": true, "expect": {"result": {"serverInfo": {"name": "caf"}}}, "timeout": 30000}}),
        json!({"mcp_list": {"expect": ["Read", "Edit", "Write", "Bash", "TaskStop"], "timeout": 30000}}),
        prompt(),
    ];
    all.extend(steps(&work));
    all.push(json!({"note": "done"}));
    let path = dir.script("script.jsonl", &after_handshake(&all));
    let mut config = common::config_for(host(), vec![fake(&dir, &path)]);
    config.policy = Box::new(AllowAll);
    config.tools.relay = Relay::Command(SpawnCommand::new(caf_fake_agent::binary()));
    config.tools.folder = Some(dir.path("s"));
    change(&mut config);
    let (caf, running) = Caf::start(config);
    Own {
        dir,
        work,
        caf,
        running: Some(running),
    }
}

impl Own {
    /// Opens a chat with all of CAF's tools in the test's folder, and
    /// sends it a message.
    async fn chat(&self) -> ChatHandle {
        let open = OpenChat::new("fake", &self.work).with_tools(Tools::caf(CafTool::ALL));
        let chat = self.caf.open_chat(open).await.unwrap();
        chat.send(UserMessage::text("Go")).await.unwrap();
        chat
    }

    /// Waits for the fake's script to end, or a check of it to fail.
    async fn script_done(&self) {
        let ended = tokio::time::timeout(PATIENCE, async {
            loop {
                let record = self.dir.record();
                if let Some(failed) = record.iter().find(|line| line.get("failed").is_some()) {
                    panic!("the fake's check failed: {failed}");
                }
                if record.iter().any(|line| line["note"] == "done") {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            ended.is_ok(),
            "the script didn't end: {:#?}",
            self.dir.record()
        );
    }

    /// The texts of the prompts the agent was sent, in order.
    fn prompts(&self) -> Vec<String> {
        (self.dir.record().iter())
            .filter(|line| line["received"]["method"] == "session/prompt")
            .map(|line| {
                (line["received"]["params"]["prompt"]
                    .as_array()
                    .unwrap()
                    .iter())
                .filter_map(|block| block["text"].as_str())
                .collect()
            })
            .collect()
    }

    /// The folder output files are saved in.
    fn out(&self) -> PathBuf {
        self.dir.path("s").join("out")
    }

    /// Every output file saved, with what's in it.
    fn saved(&self) -> Vec<(PathBuf, String)> {
        let mut saved = Vec::new();
        let within = |folder: &Path| {
            std::fs::read_dir(folder)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .collect::<Vec<_>>()
        };
        // Each process's folder, then each chat's.
        for process in within(&self.out()) {
            for chat in within(&process) {
                for file in within(&chat) {
                    let text = std::fs::read_to_string(&file).unwrap_or_default();
                    saved.push((file, text));
                }
            }
        }
        saved
    }

    async fn shut_down(mut self) {
        common::shut_down(&self.caf, self.running.take().unwrap()).await;
    }
}

/// Calls CAF's `tool` with `arguments`, as call `id`, and checks its
/// answer is `expect`.
fn call(tool: &str, id: &str, arguments: Value, expect: Value) -> [Value; 2] {
    [
        json!({"mcp_call": {"tool": tool, "arguments": arguments, "tool_use_id": id, "report": "s1", "as": id}}),
        json!({"mcp_await": {"to": id, "expect": {"result": expect}, "timeout": 30000}}),
    ]
}

/// An answer of one text.
fn says(text: &str, error: bool) -> Value {
    json!({"isError": error, "content": [{"type": "text", "text": text}]})
}

/// The chat's calls to CAF's tools, in order.
fn calls(chat: &Chat) -> Vec<&ToolCall> {
    (chat.turns.iter().flat_map(|turn| &turn.items))
        .filter_map(|item| match &item.kind {
            ItemKind::Tool(tool) => Some(&**tool),
            _ => None,
        })
        .collect()
}

fn ended(call: &ToolCall) -> bool {
    !matches!(
        call.status,
        ToolStatus::Pending | ToolStatus::Asking | ToolStatus::Running
    )
}

fn path_of(work: &Path, name: &str) -> String {
    work.join(name).display().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_edit_write_and_bash_work_on_the_disk_as_claude_codes_do() {
    let own = serve(|work| {
        std::fs::write(work.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(work.join("unread.txt"), "x\n").unwrap();
        std::fs::create_dir(work.join("sub")).unwrap();
        let a = path_of(work, "a.txt");
        let made = path_of(work, "new/b.txt");
        let unread = path_of(work, "unread.txt");
        [
            call(
                "Read",
                "r1",
                json!({"file_path": a}),
                says("1\tone\n2\ttwo", false),
            ),
            call(
                "Edit",
                "e1",
                json!({"file_path": a, "old_string": "two", "new_string": "three"}),
                says(
                    &format!("The file {a} has been updated successfully.{CURRENT}"),
                    false,
                ),
            ),
            call(
                "Write",
                "w1",
                json!({"file_path": made, "content": "made\n"}),
                says(
                    &format!("File created successfully at: {made}{CURRENT}"),
                    false,
                ),
            ),
            // Never read: its preview fails, and no one's asked.
            call(
                "Edit",
                "e2",
                json!({"file_path": unread, "old_string": "x", "new_string": "y"}),
                says(
                    "File has not been read yet. Read it first before writing to it.",
                    true,
                ),
            ),
            call(
                "Read",
                "r2",
                json!({"file_path": path_of(work, "none.txt")}),
                says(
                    &format!(
                        "File does not exist. Note: your current working directory is {}.",
                        work.display()
                    ),
                    true,
                ),
            ),
            call(
                "Bash",
                "b1",
                json!({"command": "cd sub && echo hi"}),
                says("hi", false),
            ),
            // It runs where the last one's shell ended.
            call(
                "Bash",
                "b2",
                json!({"command": "pwd", "description": "Where"}),
                says(&path_of(work, "sub"), false),
            ),
            call(
                "Bash",
                "b3",
                json!({"command": "echo out; exit 3"}),
                says("Exit code 3\nout", true),
            ),
            call(
                "Bash",
                "b4",
                json!({"command": "true"}),
                says("(Bash completed with no output)", false),
            ),
            [end_turn(), json!({"note": "turn ended"})],
        ]
        .concat()
    });
    let chat = own.chat().await;
    let mut watch = chat.watch();
    own.script_done().await;
    let done = until(&chat, &mut watch, "every call ended", |chat| {
        calls(chat).len() == 9 && calls(chat).iter().all(|call| ended(call))
    })
    .await;
    let all = calls(&done);
    let statuses: Vec<ToolStatus> = all.iter().map(|call| call.status).collect();
    use ToolStatus::{Done, Failed};
    assert_eq!(
        statuses,
        [Done, Done, Done, Failed, Failed, Done, Done, Failed, Done]
    );
    assert_eq!(
        std::fs::read_to_string(own.work.join("a.txt")).unwrap(),
        "one\nthree\n"
    );
    assert_eq!(
        std::fs::read_to_string(own.work.join("new/b.txt")).unwrap(),
        "made\n"
    );
    assert_eq!(
        std::fs::read_to_string(own.work.join("unread.txt")).unwrap(),
        "x\n"
    );
    // The edit's record is its diff, and the commands' their output,
    // ended.
    assert!(
        all[1]
            .result
            .iter()
            .any(|piece| matches!(piece, Piece::Diff(_)))
    );
    let output = |call: &ToolCall| {
        (call.result.iter()).find_map(|piece| match piece {
            Piece::Output(output) => Some(output.clone()),
            _ => None,
        })
    };
    let hi = output(all[5]).unwrap();
    assert_eq!(hi.lines, ["hi"]);
    assert_eq!(hi.ending, Some(Ending::Exited(Exit::Code(0))));
    assert_eq!(
        output(all[7]).unwrap().ending,
        Some(Ending::Exited(Exit::Code(3)))
    );
    own.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_job_answers_at_once_its_end_is_a_notice_and_task_stop_stops_one() {
    let own = serve(|work| {
        let go = work.join("go").display().to_string();
        let quick = json!({"command": "sleep 0.5; echo done", "run_in_background": true, "description": "Quick one"});
        let long = json!({"command": "echo started; sleep 1000", "run_in_background": true});
        [
            vec![
                json!({"mcp_call": {"tool": "Bash", "arguments": quick, "tool_use_id": "b1", "report": "s1", "as": "b1"}}),
                json!({"mcp_await": {"to": "b1", "expect": {"result": {"isError": false}}, "timeout": 30000}}),
                json!({"mcp_call": {"tool": "Bash", "arguments": long, "tool_use_id": "b2", "report": "s1", "as": "b2"}}),
                json!({"mcp_await": {"to": "b2", "expect": {"result": {"isError": false}}, "timeout": 30000}}),
                // Once the test has seen its file grow.
                json!({"wait_file": {"path": go, "timeout": 30000}}),
            ],
            call("TaskStop", "t1", json!({"task_id": "2"}), says("Stopped task 2", false)).to_vec(),
            call(
                "TaskStop",
                "t2",
                json!({"task_id": "9"}),
                says("Refused: No task 9 in this conversation", true),
            )
            .to_vec(),
            vec![
                end_turn(),
                // The first job's end, as a notice turn.
                json!({"wait": {"method": "session/prompt", "as": "notice", "timeout": 30000}}),
                json!({"respond": {"to": "notice", "result": {"stopReason": "end_turn"}}}),
            ],
        ]
        .concat()
    });
    let chat = own.chat().await;
    let mut watch = chat.watch();
    // The running job's file has what it printed, as it runs.
    tokio::time::timeout(PATIENCE, async {
        while !own.saved().iter().any(|(_, text)| text == "started\n") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the job's file grew");
    std::fs::write(own.work.join("go"), "").unwrap();
    own.script_done().await;
    let done = until(&chat, &mut watch, "both jobs ended", |chat| {
        let all = calls(chat);
        all.len() == 4 && all.iter().all(|call| ended(call))
    })
    .await;
    let all = calls(&done);
    assert_eq!(
        (all[0].status, all[0].job),
        (ToolStatus::Done, Some(JobId(1)))
    );
    assert_eq!(
        (all[1].status, all[1].job, all[1].stopped_by),
        (
            ToolStatus::Cancelled,
            Some(JobId(2)),
            Some(StoppedBy::Agent)
        )
    );
    assert_eq!(all[2].status, ToolStatus::Done);
    let prompts = own.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:#?}");
    let notice = &prompts[1];
    assert!(notice.contains("<task-id>1</task-id>"), "{notice}");
    assert!(notice.contains("<tool-use-id>b1</tool-use-id>"), "{notice}");
    assert!(
        notice.contains("Background command \"Quick one\" completed (exit code 0)"),
        "{notice}"
    );
    // Its output is in the file the notice names, which is the user's
    // alone.
    let saved = own.saved();
    let quick = saved
        .iter()
        .find(|(path, _)| notice.contains(&path.display().to_string()))
        .expect("the notice names a file saved");
    assert_eq!(quick.1, "done\n");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&quick.0).unwrap().permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);
    let stopped = saved.iter().find(|(path, _)| path != &quick.0).unwrap();
    assert_eq!(stopped.1, "started\n");
    // Shutting down takes them away.
    let out = own.out();
    own.shut_down().await;
    assert!(
        !out.exists(),
        "{:?}",
        std::fs::read_dir(&out).map(|dir| dir.count())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_past_its_timeout_goes_on_in_the_background() {
    let own = serve(|_| {
        let slow = json!({"command": "echo first; sleep 1; echo late", "timeout": 200});
        vec![
            json!({"mcp_call": {"tool": "Bash", "arguments": slow, "tool_use_id": "b1", "report": "s1", "as": "b1"}}),
            json!({"mcp_await": {"to": "b1", "expect": {"result": {"isError": false}}, "timeout": 30000}}),
            end_turn(),
            json!({"wait": {"method": "session/prompt", "as": "notice", "timeout": 30000}}),
            json!({"respond": {"to": "notice", "result": {"stopReason": "end_turn"}}}),
        ]
    });
    let chat = own.chat().await;
    let mut watch = chat.watch();
    own.script_done().await;
    let done = until(&chat, &mut watch, "the job ended", |chat| {
        calls(chat).first().is_some_and(|call| ended(call))
    })
    .await;
    let call = calls(&done)[0];
    assert_eq!((call.status, call.job), (ToolStatus::Done, Some(JobId(1))));
    let notice = &own.prompts()[1];
    assert!(notice.contains("completed (exit code 0)"), "{notice}");
    let saved = own.saved();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].1, "first\nlate\n");
    own.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_output_too_large_to_answer_is_saved_whole_and_the_model_given_its_start() {
    let own = serve(|_| {
        [
            vec![json!({"mcp_call": {"tool": "Bash", "arguments": {"command": "seq 1 100000"}, "tool_use_id": "b1", "report": "s1", "as": "b1"}})],
            vec![json!({"mcp_await": {"to": "b1", "expect": {"result": {"isError": false}}, "timeout": 30000}})],
            vec![end_turn()],
        ]
        .concat()
    });
    let chat = own.chat().await;
    let mut watch = chat.watch();
    own.script_done().await;
    until(&chat, &mut watch, "the call ended", |chat| {
        calls(chat).first().is_some_and(|call| ended(call))
    })
    .await;
    let saved = own.saved();
    assert_eq!(saved.len(), 1);
    let expected: String = (1..=100000).map(|n| format!("{n}\n")).collect();
    assert_eq!(saved[0].1, expected);
    own.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_turn_stops_its_command() {
    let own = serve(|work| {
        let pid = work.join("pid").display().to_string();
        let command = format!("echo $$ > {pid}; sleep 1000");
        vec![
            json!({"mcp_call": {"tool": "Bash", "arguments": {"command": command}, "tool_use_id": "b1", "report": "s1", "as": "b1"}}),
            json!({"on": {"method": "session/cancel", "do": [{"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}]}}),
            json!({"mcp_await": {"to": "b1", "expect": {"result": {"isError": true}}, "timeout": 30000}}),
        ]
    });
    let chat = own.chat().await;
    let pid_file = own.work.join("pid");
    let pid: i32 = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the command started");
    chat.stop().await.unwrap();
    own.script_done().await;
    tokio::time::timeout(PATIENCE, async {
        #[allow(unsafe_code)]
        // SAFETY: a plain system call on numbers.
        while unsafe { libc_kill(pid) } {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the command stopped");
    own.shut_down().await;
}

/// Whether process `pid` is still there.
#[allow(unsafe_code)]
unsafe fn libc_kill(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: signal 0 only asks whether it's there.
    unsafe { kill(pid, 0) == 0 }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_picture_is_read_as_an_image() {
    let own = serve(|work| {
        let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0; 8]].concat();
        std::fs::write(work.join("a.png"), png).unwrap();
        let image = json!({"isError": false, "content": [{"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgoAAAAAAAAAAA=="}]});
        [
            call(
                "Read",
                "r1",
                json!({"file_path": path_of(work, "a.png")}),
                image,
            )
            .to_vec(),
            vec![end_turn()],
        ]
        .concat()
    });
    let chat = own.chat().await;
    let mut watch = chat.watch();
    own.script_done().await;
    let done = until(&chat, &mut watch, "the call ended", |chat| {
        calls(chat).first().is_some_and(|call| ended(call))
    })
    .await;
    let call = calls(&done)[0];
    assert_eq!(call.status, ToolStatus::Done);
    assert!(
        call.result
            .iter()
            .any(|piece| matches!(piece, Piece::Image(image) if image.mime == "image/png")),
        "{:?}",
        call.result
    );
    own.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_runs_at_most_eight_jobs_and_closing_it_stops_them() {
    let own = serve(|_| {
        let mut steps = Vec::new();
        for n in 1..=8 {
            let id = format!("b{n}");
            let arguments = json!({"command": "sleep 1000", "run_in_background": true});
            steps.push(json!({"mcp_call": {"tool": "Bash", "arguments": arguments, "tool_use_id": id, "report": "s1", "as": id}}));
            steps.push(json!({"mcp_await": {"to": id, "expect": {"result": {"isError": false}}, "timeout": 30000}}));
        }
        steps.extend(call(
            "Bash",
            "b9",
            json!({"command": "sleep 1000", "run_in_background": true}),
            says(
                "Too many background tasks running (8); stop one with TaskStop first.",
                true,
            ),
        ));
        steps.push(end_turn());
        steps
    });
    let chat = own.chat().await;
    let mut watch = chat.watch();
    own.script_done().await;
    until(&chat, &mut watch, "eight jobs running", |chat| {
        let all = calls(chat);
        all.len() == 9 && all.iter().filter(|call| call.job.is_some()).count() == 8
    })
    .await;
    assert_eq!(own.saved().len(), 8);
    let id = chat.id;
    chat.close().await.unwrap();
    let chat = own.caf.chat(id);
    // Closed, its jobs are stopped, by the user.
    let closed = until(&chat, &mut watch, "its jobs stopped", |chat| {
        calls(chat)
            .iter()
            .filter(|call| call.job.is_some())
            .all(|call| ended(call))
    })
    .await;
    assert!(
        (calls(&closed).iter())
            .filter(|call| call.job.is_some())
            .all(|call| call.status == ToolStatus::Cancelled
                && call.stopped_by == Some(StoppedBy::User))
    );
    // Their files go when it's let go, or CAF ends.
    let out = own.out();
    own.shut_down().await;
    assert!(!out.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_progress_bar_is_recorded_as_its_last_drawing() {
    let own = serve(|_| {
        let command = "echo start; for n in 10 50 100; do printf '\\r\\033[32m%s%%\\033[0m' $n; sleep 0.05; done; echo";
        [
            call(
                "Bash",
                "b1",
                json!({"command": command}),
                says("start\n100%", false),
            )
            .to_vec(),
            vec![end_turn()],
        ]
        .concat()
    });
    let chat = own.chat().await;
    let mut watch = chat.watch();
    own.script_done().await;
    let done = until(&chat, &mut watch, "the call ended", |chat| {
        calls(chat).first().is_some_and(|call| ended(call))
    })
    .await;
    let output = (calls(&done)[0].result.iter())
        .find_map(|piece| match piece {
            Piece::Output(output) => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    // Its colours are kept for the user; the model has none.
    assert_eq!(output.lines, ["start", "\u{1b}[0;32m100%\u{1b}[0m"]);
    own.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shell_folder_thats_gone_starts_again_in_the_chats() {
    let own = serve(|work| {
        let gone = path_of(work, "gone");
        let here = work.display().to_string();
        [
            call("Bash", "b1", json!({"command": "mkdir gone && cd gone"}), says("(Bash completed with no output)", false)).to_vec(),
            // Its shell ends in a folder that's no longer there.
            call("Bash", "b2", json!({"command": "rmdir \"$PWD\""}), says("(Bash completed with no output)", false)).to_vec(),
            call(
                "Bash",
                "b3",
                json!({"command": "pwd"}),
                says(&format!("The shell's folder, {gone}, no longer exists, so this ran in {here} instead.\n{here}"), false),
            )
            .to_vec(),
            // And it goes on there.
            call("Bash", "b4", json!({"command": "pwd"}), says(&here, false)).to_vec(),
            vec![end_turn()],
        ]
        .concat()
    });
    let _chat = own.chat().await;
    own.script_done().await;
    own.shut_down().await;
}

/// An app's terminal whose output stays open after its command ends, as
/// a visible terminal can: it prints one line, then nothing, ever.
struct OpenTerminal;

/// A command in an [`OpenTerminal`].
struct InOpenTerminal {
    printed: bool,
}

impl caf_driver_tokio::caf_tools::Commands for OpenTerminal {
    fn start(
        &self,
        _spec: caf_driver_tokio::caf_tools::CommandSpec,
    ) -> caf_driver_tokio::caf_tools::BoxFuture<
        Result<Box<dyn caf_driver_tokio::caf_tools::Running>, String>,
    > {
        Box::pin(std::future::ready(Ok(
            Box::new(InOpenTerminal { printed: false })
                as Box<dyn caf_driver_tokio::caf_tools::Running>,
        )))
    }
}

impl caf_driver_tokio::caf_tools::Running for InOpenTerminal {
    fn output(
        &mut self,
    ) -> caf_driver_tokio::caf_tools::BoxFuture<Option<caf_driver_tokio::caf_tools::Output>> {
        if std::mem::replace(&mut self.printed, true) {
            return Box::pin(std::future::pending());
        }
        let line = caf_driver_tokio::caf_tools::Output::Bytes(b"done\r\n".to_vec());
        Box::pin(std::future::ready(Some(line)))
    }

    fn wait(
        &mut self,
    ) -> caf_driver_tokio::caf_tools::BoxFuture<caf_driver_tokio::caf_tools::Ended> {
        let ended = caf_driver_tokio::caf_tools::Ended::new(Some(Exit::Code(0)), None);
        Box::pin(std::future::ready(ended))
    }

    fn interrupt(&mut self) {}

    fn stop(&mut self) -> caf_driver_tokio::caf_tools::BoxFuture<()> {
        Box::pin(std::future::ready(()))
    }

    fn place(&self) -> Option<String> {
        Some("terminal 7".to_owned())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_ends_when_it_does_though_its_terminal_stays_open() {
    let own = serve_with(
        |_| {
            [
                call(
                    "Bash",
                    "b1",
                    json!({"command": "make"}),
                    says("Ran in terminal 7\ndone", false),
                )
                .to_vec(),
                vec![end_turn()],
            ]
            .concat()
        },
        |config| config.tools.commands = std::sync::Arc::new(OpenTerminal),
    );
    let _chat = own.chat().await;
    own.script_done().await;
    own.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_that_ended_isnt_moved_to_the_background_while_its_output_is_taken() {
    // It ends at once; its timeout comes while what's left of its output
    // is still being taken.
    let own = serve_with(
        |_| {
            [
                call(
                    "Bash",
                    "b1",
                    json!({"command": "make", "timeout": 200}),
                    says("Ran in terminal 7\ndone", false),
                )
                .to_vec(),
                vec![end_turn()],
            ]
            .concat()
        },
        |config| config.tools.commands = std::sync::Arc::new(OpenTerminal),
    );
    let _chat = own.chat().await;
    own.script_done().await;
    own.shut_down().await;
}

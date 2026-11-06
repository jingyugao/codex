//! End-to-end turn handoff between two app-server processes.
//!
//! Requires the binary: `cargo build -p codex-app-server --bin codex-app-server`.

#![cfg(unix)]

use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCRequest;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use core_test_support::responses;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::time::Instant;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_hands_off_running_turn_to_next_server_with_its_env() -> Result<()> {
    let server = responses::start_mock_server().await;
    // Source server: the first sampling request never completes before SIGTERM.
    mount(
        &server,
        1,
        final_message("too late"),
        Duration::from_secs(120),
    )
    .await;
    // Target server: the recovered turn echoes the per-turn env, then finishes.
    let echo = responses::sse(vec![
        responses::ev_response_created("resp-2"),
        responses::ev_exec_command_call("call-echo", "echo fork-env=$TRACE_ID"),
        responses::ev_completed("resp-2"),
    ]);
    mount(&server, 2, echo, Duration::ZERO).await;
    mount(&server, 3, final_message("recovered"), Duration::ZERO).await;

    let codex_home = TempDir::new()?;
    let handoff_dir = TempDir::new()?;
    write_config(codex_home.path(), &server.uri())?;

    // Pod A: start a turn carrying fork env, then SIGTERM it mid-sampling.
    let (mut pod_a, addr) = spawn_server(codex_home.path(), handoff_dir.path(), "pod-a").await?;
    let mut ws = connect(addr).await?;
    initialize(&mut ws).await?;
    let thread = request(
        &mut ws,
        2,
        "thread/start",
        serde_json::to_value(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })?,
    )
    .await?;
    let thread_id = thread.result["thread"]["id"]
        .as_str()
        .context("thread id")?
        .to_string();
    let turn = request(
        &mut ws,
        3,
        "turn/start",
        serde_json::to_value(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            responsesapi_client_metadata: Some(
                [("fork.env.TRACE_ID".to_string(), "trace-123".to_string())].into(),
            ),
            ..Default::default()
        })?,
    )
    .await?;
    let turn_id = turn.result["turn"]["id"]
        .as_str()
        .context("turn id")?
        .to_string();
    wait_for_requests(&server, 1).await?;

    kill(&pod_a, "-TERM")?;
    let status = timeout(Duration::from_secs(20), pod_a.wait())
        .await
        .context("pod A did not exit after handing off")??;
    assert!(status.success(), "pod A exit: {status}");
    let record: Value = serde_json::from_slice(&std::fs::read(
        handoff_dir.path().join(format!("{thread_id}.json")),
    )?)?;
    assert_eq!(record["turn_id"], json!(turn_id));
    assert_eq!(record["payload"]["env"], json!({ "TRACE_ID": "trace-123" }));

    // Pod B: resuming the thread recovers the same turn and finishes it.
    let (_pod_b, addr) = spawn_server(codex_home.path(), handoff_dir.path(), "pod-b").await?;
    let mut ws = connect(addr).await?;
    initialize(&mut ws).await?;
    request(
        &mut ws,
        2,
        "thread/resume",
        serde_json::to_value(ThreadResumeParams {
            thread_id: thread_id.clone(),
            ..Default::default()
        })?,
    )
    .await?;
    let completed = notification(&mut ws, "turn/completed").await?;
    let params = completed.params.context("turn/completed params")?;
    assert_eq!(params["turn"]["id"], json!(turn_id));
    assert_eq!(params["turn"]["status"], json!("completed"));

    let bodies = response_bodies(&server).await?;
    assert_eq!(bodies.len(), 3, "one source and two recovered requests");
    assert!(
        bodies[2].contains("fork-env=trace-123"),
        "recovered exec output should carry the per-turn env"
    );
    assert!(
        !bodies.iter().any(|body| body.contains("fork.env.")),
        "fork env metadata must not reach the model provider"
    );
    assert!(std::fs::read_dir(handoff_dir.path())?.next().is_none());
    Ok(())
}

fn final_message(text: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created("resp"),
        responses::ev_assistant_message("msg", text),
        responses::ev_completed("resp"),
    ])
}

async fn mount(server: &MockServer, priority: u8, body: String, delay: Duration) {
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(responses::sse_response(body).set_delay(delay))
        .with_priority(priority)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

fn write_config(codex_home: &Path, server_uri: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "read-only"
model_provider = "mock_provider"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}

async fn spawn_server(
    codex_home: &Path,
    handoff_dir: &Path,
    owner: &str,
) -> Result<(Child, SocketAddr)> {
    let program = codex_utils_cargo_bin::cargo_bin("codex-app-server")
        .context("build codex-app-server first")?;
    let mut child = Command::new(program)
        .args(["--listen", "ws://127.0.0.1:0"])
        .arg("--disable-plugin-startup-tasks-for-tests")
        .env("CODEX_HOME", codex_home)
        .env("RUST_LOG", "warn,codex_fork_ext=info")
        .env(
            codex_fork_ext::HANDOFF_STORE_ENV_VAR,
            format!("file:{}", handoff_dir.display()),
        )
        .env(codex_fork_ext::HANDOFF_ON_SIGTERM_ENV_VAR, "1")
        .env(codex_fork_ext::HANDOFF_GRACE_SECS_ENV_VAR, "0")
        .env("CODEX_FORK_OWNER_ID", owner)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stderr = BufReader::new(child.stderr.take().context("stderr")?).lines();
    let addr = loop {
        let line = timeout(READ_TIMEOUT, stderr.next_line())
            .await
            .context("waiting for bind address")??
            .context("app-server exited before binding")?;
        eprintln!("[{owner}] {line}");
        if let Some(addr) = strip_ansi(&line)
            .split_whitespace()
            .find_map(|word| word.strip_prefix("ws://")?.parse::<SocketAddr>().ok())
        {
            break addr;
        }
    };
    let owner = owner.to_string();
    tokio::spawn(async move {
        while let Ok(Some(line)) = stderr.next_line().await {
            eprintln!("[{owner}] {line}");
        }
    });
    Ok((child, addr))
}

fn strip_ansi(line: &str) -> String {
    let mut stripped = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        stripped.push(ch);
    }
    stripped
}

async fn connect(addr: SocketAddr) -> Result<Ws> {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}")).await?;
    Ok(ws)
}

async fn initialize(ws: &mut Ws) -> Result<()> {
    request(
        ws,
        1,
        "initialize",
        json!({
            "clientInfo": { "name": "fork-handoff-e2e", "version": "0.1.0" },
            "capabilities": { "experimentalApi": true },
        }),
    )
    .await?;
    Ok(())
}

async fn request(ws: &mut Ws, id: i64, method: &str, params: Value) -> Result<JSONRPCResponse> {
    let message = JSONRPCMessage::Request(JSONRPCRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
        trace: None,
    });
    ws.send(Message::Text(serde_json::to_string(&message)?.into()))
        .await?;
    loop {
        match read(ws).await? {
            JSONRPCMessage::Response(response) if response.id == RequestId::Integer(id) => {
                return Ok(response);
            }
            JSONRPCMessage::Error(error) if error.id == RequestId::Integer(id) => {
                bail!("{method} failed: {:?}", error.error);
            }
            _ => {}
        }
    }
}

async fn notification(ws: &mut Ws, method: &str) -> Result<JSONRPCNotification> {
    loop {
        if let JSONRPCMessage::Notification(notification) = read(ws).await?
            && notification.method == method
        {
            return Ok(notification);
        }
    }
}

async fn read(ws: &mut Ws) -> Result<JSONRPCMessage> {
    loop {
        let frame = timeout(READ_TIMEOUT, ws.next())
            .await
            .context("timed out waiting for websocket frame")?
            .context("websocket closed")??;
        if let Message::Text(text) = frame {
            return Ok(serde_json::from_str(text.as_ref())?);
        }
    }
}

async fn wait_for_requests(server: &MockServer, count: usize) -> Result<()> {
    let deadline = Instant::now() + READ_TIMEOUT;
    while response_bodies(server).await?.len() < count {
        if Instant::now() >= deadline {
            bail!("timed out waiting for {count} /responses request(s)");
        }
        sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

async fn response_bodies(server: &MockServer) -> Result<Vec<String>> {
    Ok(server
        .received_requests()
        .await
        .context("mock server requests")?
        .into_iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect())
}

fn kill(child: &Child, signal: &str) -> Result<()> {
    let pid = child.id().context("pid")?.to_string();
    let status = std::process::Command::new("kill")
        .args([signal, &pid])
        .status()?;
    anyhow::ensure!(status.success(), "kill {signal} failed");
    Ok(())
}

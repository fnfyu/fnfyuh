use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn stop_during_model_request_responds_before_model_returns_and_prevents_proposals() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let header = String::from_utf8(headers).unwrap();
        let length: usize = header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse().unwrap())).unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let body = json!({"choices":[{"message":{"content":"writing","tool_calls":[{"id":"write-1","type":"function","function":{"name":"write","arguments":"{\"path\":\"/tmp/never-written\",\"content\":\"oops\"}"}}]},"finish_reason":"tool_calls"}]}).to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let directory = std::env::temp_dir().join(format!("harness-stop-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let settings = directory.join("settings.json");
    std::fs::write(&settings, json!({"version":1,"default_model":"fixture","providers":[{"id":"fixture","name":"Fixture","kind":"open_ai_compatible","endpoint":endpoint,"models":[{"id":"fixture","label":"Fixture","provider_id":"fixture"}]}]}).to_string()).unwrap();
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .env("HARNESS_DB", directory.join("events.sqlite"))
        .env("HARNESS_SETTINGS", &settings)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let mut stdin = daemon.stdin.take().unwrap();
    let stdout = daemon.stdout.take().unwrap();
    let (output_tx, output_rx) = mpsc::channel::<Value>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            output_tx.send(serde_json::from_str(&line.unwrap()).unwrap()).unwrap();
        }
    });
    let send = |input: Value, stdin: &mut std::process::ChildStdin| {
        writeln!(stdin, "{input}").unwrap();
        stdin.flush().unwrap();
    };
    let response = |id: i64| -> Value {
        loop {
            let value = output_rx.recv_timeout(Duration::from_secs(5)).expect("daemon did not respond in time");
            if value["id"] == id { return value; }
        }
    };
    send(json!({"jsonrpc":"2.0","id":1,"method":"runtime.v1.session.create","params":{"workspace_roots":[directory.to_str().unwrap()]}}), &mut stdin);
    let session_id = response(1)["result"]["session_id"].as_str().unwrap().to_owned();
    send(json!({"jsonrpc":"2.0","id":2,"method":"runtime.v1.turn.start","params":{"session_id":session_id,"turn_id":"target","content":"write it","model":"fixture"}}), &mut stdin);
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("model was never called");
    send(json!({"jsonrpc":"2.0","id":3,"method":"runtime.v1.turn.stop","params":{"session_id":session_id,"turn_id":"target"}}), &mut stdin);
    let stopped = response(3);
    assert_eq!(stopped["result"]["accepted"], true);
    assert_eq!(stopped["result"]["stopped"], false, "stopped means durable terminal state, not cancellation requested");
    release_tx.send(()).unwrap();
    let start_result = response(2);
    assert_eq!(start_result["result"]["accepted"], false);
    send(json!({"jsonrpc":"2.0","id":4,"method":"runtime.v1.session.events","params":{"session_id":session_id}}), &mut stdin);
    let events = response(4);
    let payloads = events["result"]["events"].as_array().expect("events RPC");
    let serialized = serde_json::to_string(payloads).unwrap();
    assert!(serialized.contains("RunCompleted") && serialized.contains("RecoveryRequired"), "{serialized}");
    assert!(!serialized.contains("ToolProposed") && !serialized.contains("ToolStarted"), "{serialized}");
    drop(stdin);
    assert!(daemon.wait().unwrap().success());
    reader.join().unwrap();
    server.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

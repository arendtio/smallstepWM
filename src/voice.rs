//! OpenAI Realtime session. Off means no socket and no microphone.
//!
//! F1 and the strip entry are a toggle, not hold-to-talk. While the session is on,
//! semantic VAD on the server separates utterances. The key is read from
//! `OPENAI_API_KEY` or `~/.credentials_openai` and is never logged.

use std::{
    io::Read,
    net::ToSocketAddrs,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use smithay::reexports::calloop::channel::Sender;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

use crate::tools;

const MODEL: &str = "gpt-realtime-mini";

#[derive(Clone, Debug)]
pub struct Session {
    pub connecting: bool,
    pub live: bool,
    pub note: String,
    pub generation: u64,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            connecting: false,
            live: false,
            note: String::new(),
            generation: 0,
        }
    }
}

pub enum Report {
    Status {
        generation: u64,
        connecting: bool,
        live: bool,
        note: String,
    },
    Tool {
        generation: u64,
        name: String,
        arguments: String,
        reply: tokio::sync::oneshot::Sender<String>,
    },
}

pub fn f1_edge(held: &mut bool, pressed: bool) -> bool {
    if pressed {
        if *held {
            return false;
        }
        *held = true;
        true
    } else {
        *held = false;
        false
    }
}

pub fn load_key() -> Result<String, String> {
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(key);
        }
    }
    let home = std::env::var("HOME").map_err(|_| "No API key".to_string())?;
    let path = std::path::Path::new(&home).join(".credentials_openai");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let key = text.lines().next().unwrap_or("").trim().to_string();
            if key.is_empty() {
                Err("API key file is empty".into())
            } else {
                Ok(key)
            }
        }
        Err(_) => Err("No API key in OPENAI_API_KEY or ~/.credentials_openai".into()),
    }
}

pub fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "•••")
    }
}

pub fn model_name() -> String {
    match std::env::var("SMALLSTEP_REALTIME_MODEL") {
        Ok(raw) => {
            let raw = raw.trim();
            if !raw.is_empty()
                && raw
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
            {
                return raw.to_string();
            }
            MODEL.into()
        }
        Err(_) => MODEL.into(),
    }
}

pub fn session_update(model: &str) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "model": model,
            "output_modalities": ["audio"],
            "instructions": tools::instructions(),
            "tools": tools::definitions(),
            "tool_choice": "auto",
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": 24000 },
                    "noise_reduction": { "type": "far_field" },
                    "turn_detection": {
                        "type": "semantic_vad",
                        "eagerness": "low",
                        "create_response": true,
                        "interrupt_response": true
                    }
                },
                "output": {
                    "format": { "type": "audio/pcm", "rate": 24000 }
                }
            }
        }
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    Ignore,
    SessionCreated,
    SessionUpdated,
    Audio(Vec<u8>),
    SpeechStarted,
    Function { call_id: String, name: String, arguments: String },
    Error(String),
    ResponseDone,
}

pub fn parse_event(text: &str) -> Incoming {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Incoming::Ignore;
    };
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "session.created" => Incoming::SessionCreated,
        "session.updated" => Incoming::SessionUpdated,
        "input_audio_buffer.speech_started" => Incoming::SpeechStarted,
        "response.done" | "response.output_audio.done" => Incoming::ResponseDone,
        "response.output_audio.delta" => value
            .get("delta")
            .and_then(Value::as_str)
            .and_then(|delta| STANDARD.decode(delta).ok())
            .map(Incoming::Audio)
            .unwrap_or(Incoming::Ignore),
        "response.function_call_arguments.done" => {
            let call_id = value.get("call_id").and_then(Value::as_str).unwrap_or("").to_string();
            let name = value.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let arguments = value.get("arguments").and_then(Value::as_str).unwrap_or("{}").to_string();
            if call_id.is_empty() || name.is_empty() {
                Incoming::Ignore
            } else {
                Incoming::Function { call_id, name, arguments }
            }
        }
        "error" => {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Voice API error");
            Incoming::Error(message.chars().take(160).collect())
        }
        _ => Incoming::Ignore,
    }
}

pub fn api_reachable() -> Result<(), String> {
    let mut addrs = ("api.openai.com", 443u16)
        .to_socket_addrs()
        .map_err(|err| format!("Voice API is unreachable ({err})"))?;
    let Some(addr) = addrs.next() else {
        return Err("Voice API is unreachable".into());
    };
    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(4))
        .map(|_| ())
        .map_err(|err| format!("Voice API is unreachable ({err})"))
}

pub fn install_tls() {
    // rustls 0.23 has no process-wide provider until one is selected. The first
    // connection panics without it, and a panic used to take the whole session down.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn panic_note(payload: &(dyn std::any::Any + Send)) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    let detail: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let detail: String = detail.chars().take(80).collect();
    if detail.is_empty() {
        "Voice stopped".into()
    } else {
        format!("Voice stopped: {detail}")
    }
}

pub fn run(tx: Sender<Report>, stop: Arc<AtomicBool>, generation: u64) {
    install_tls();
    let fail = |note: String| {
        let _ = tx.send(Report::Status {
            generation,
            connecting: false,
            live: false,
            note,
        });
    };
    let key = match load_key() {
        Ok(key) => key,
        Err(note) => {
            fail(note);
            return;
        }
    };
    if let Err(note) = recorder_present() {
        fail(note);
        return;
    }
    if stop.load(Ordering::Relaxed) {
        return;
    }
    if let Err(note) = api_reachable() {
        fail(note);
        return;
    }
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            fail(format!("Voice runtime failed ({err})"));
            return;
        }
    };
    runtime.block_on(drive(tx, stop, generation, key));
}

fn recorder_present() -> Result<(), String> {
    for program in ["arecord", "aplay"] {
        match Command::new(program).arg("--help").output() {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("No {program}. Install alsa-utils."));
            }
            _ => {}
        }
    }
    Ok(())
}

async fn drive(tx: Sender<Report>, stop: Arc<AtomicBool>, generation: u64, key: String) {
    let model = model_name();
    let url = format!("wss://api.openai.com/v1/realtime?model={model}");
    let mut request = match url.into_client_request() {
        Ok(request) => request,
        Err(err) => {
            status(&tx, generation, false, false, redact(&err.to_string(), &key));
            return;
        }
    };
    let header = match HeaderValue::from_str(&format!("Bearer {key}")) {
        Ok(header) => header,
        Err(_) => {
            status(&tx, generation, false, false, "API key has an invalid character".into());
            return;
        }
    };
    request.headers_mut().insert("Authorization", header);
    let socket = match connect_async(request).await {
        Ok((socket, _)) => socket,
        Err(err) => {
            status(&tx, generation, false, false, redact(&err.to_string(), &key));
            return;
        }
    };
    let (mut write, mut read) = socket.split();
    let (play_tx, play_rx) = std::sync::mpsc::channel();
    let playback = std::thread::Builder::new()
        .name("voice-play".into())
        .spawn(move || playback_loop(play_rx))
        .ok();
    let mut capture: Option<Capture> = None;
    let mut live = false;
    let mut ending = false;
    let mut end_settled = false;
    let mut last_audio = Instant::now();
    let mut played_until = Instant::now();
    let mut ending_since: Option<Instant> = None;
    let mut configured = false;
    let mut failure: Option<String> = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(started) = ending_since {
            let playback_idle = Instant::now() > played_until + Duration::from_millis(400);
            let quiet = last_audio.elapsed() > Duration::from_millis(1500);
            let settled = end_settled || (started.elapsed() > Duration::from_millis(1500) && quiet);
            if started.elapsed() > Duration::from_secs(12) || (settled && playback_idle) {
                break;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            incoming = read.next() => {
                let Some(Ok(message)) = incoming else { break };
                let Message::Text(text) = message else { continue };
                match parse_event(text.as_str()) {
                    Incoming::SessionCreated if !configured => {
                        configured = true;
                        let update = session_update(&model).to_string();
                        if write.send(Message::Text(update.into())).await.is_err() {
                            failure = Some("Voice session closed".into());
                            break;
                        }
                    }
                    Incoming::SessionUpdated if !live && !ending => {
                        match start_capture(&stop) {
                            Ok(started) => {
                                capture = Some(started);
                                live = true;
                                status(&tx, generation, false, true, "Live".into());
                            }
                            Err(note) => {
                                failure = Some(note);
                                break;
                            }
                        }
                    }
                    Incoming::Audio(pcm) => {
                        last_audio = Instant::now();
                        let samples = (pcm.len() / 2) as u64;
                        let base = played_until.max(Instant::now());
                        played_until = base + Duration::from_millis(samples.saturating_mul(1000) / 24_000);
                        let _ = play_tx.send(PlayMsg::Pcm(pcm));
                    }
                    Incoming::SpeechStarted => {
                        played_until = Instant::now();
                        let _ = play_tx.send(PlayMsg::Reset);
                    }
                    Incoming::Function { call_id, name, arguments } => {
                        if name == "end_voice_session" {
                            ending = true;
                            end_settled = false;
                            ending_since = Some(Instant::now());
                        }
                        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                        if tx.send(Report::Tool { generation, name: name.clone(), arguments, reply: reply_tx }).is_err() {
                            break;
                        }
                        let output = reply_rx.await.unwrap_or_else(|_| "tool failed".into());
                        let item = json!({
                            "type": "conversation.item.create",
                            "item": { "type": "function_call_output", "call_id": call_id, "output": output }
                        });
                        if write.send(Message::Text(item.to_string().into())).await.is_err() {
                            failure = Some("Voice session closed".into());
                            break;
                        }
                        let speak = name != "wait_for_user" && name != "end_voice_session";
                        if speak && write.send(Message::Text(r#"{"type":"response.create"}"#.into())).await.is_err() {
                            failure = Some("Voice session closed".into());
                            break;
                        }
                        if live && !ending {
                            status(&tx, generation, false, true, "Live".into());
                        }
                    }
                    Incoming::Error(note) if !live => {
                        failure = Some(redact(&note, &key));
                        break;
                    }
                    Incoming::Error(note) => {
                        status(&tx, generation, false, true, redact(&note, &key));
                    }
                    Incoming::ResponseDone => {
                        if ending {
                            end_settled = true;
                        }
                    }
                    Incoming::Ignore | Incoming::SessionCreated | Incoming::SessionUpdated => {}
                }
            }
        }
        if let Some(capture) = capture.as_mut() {
            while let Ok(pcm) = capture.audio.try_recv() {
                if !live || ending {
                    break;
                }
                let event = json!({
                    "type": "input_audio_buffer.append",
                    "audio": STANDARD.encode(&pcm)
                });
                if write.send(Message::Text(event.to_string().into())).await.is_err() {
                    live = false;
                    break;
                }
            }
        }
    }

    drop(capture);
    let _ = play_tx.send(PlayMsg::Quit);
    if let Some(playback) = playback {
        let _ = playback.join();
    }
    if !stop.load(Ordering::Relaxed) {
        let note = match failure {
            Some(note) => note,
            None if live && !ending => "Connection closed".into(),
            None => String::new(),
        };
        status(&tx, generation, false, false, note);
    }
}

fn status(tx: &Sender<Report>, generation: u64, connecting: bool, live: bool, note: String) {
    let _ = tx.send(Report::Status {
        generation,
        connecting,
        live,
        note,
    });
}

struct Capture {
    child: Arc<Mutex<std::process::Child>>,
    audio: std::sync::mpsc::Receiver<Vec<u8>>,
    stop_capture: Arc<AtomicBool>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop_capture.store(true, Ordering::Relaxed);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn start_capture(session_stop: &Arc<AtomicBool>) -> Result<Capture, String> {
    let mut child = Command::new("arecord")
        .args(["-q", "-D", "default", "-f", "S16_LE", "-r", "24000", "-c", "1", "-t", "raw"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                "No arecord. Install alsa-utils.".into()
            } else {
                format!("Microphone failed ({err})")
            }
        })?;
    let mut stdout = child.stdout.take().ok_or_else(|| "Microphone has no output".to_string())?;
    let child = Arc::new(Mutex::new(child));
    let stop_capture = Arc::new(AtomicBool::new(false));
    let killer = Arc::clone(&child);
    let stop_flag = Arc::clone(&stop_capture);
    let session = Arc::clone(session_stop);
    std::thread::spawn(move || {
        while !stop_flag.load(Ordering::Relaxed) && !session.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Ok(mut child) = killer.lock() {
            let _ = child.kill();
        }
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("voice-mic".into())
        .spawn(move || {
            let mut buf = [0u8; 4800];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let even = n - (n % 2);
                        if even > 0 && tx.send(buf[..even].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .map_err(|err| format!("Microphone thread failed ({err})"))?;
    Ok(Capture {
        child,
        audio: rx,
        stop_capture,
    })
}

enum PlayMsg {
    Pcm(Vec<u8>),
    Reset,
    Quit,
}

fn playback_loop(rx: std::sync::mpsc::Receiver<PlayMsg>) {
    let mut child: Option<std::process::Child> = None;
    while let Ok(message) = rx.recv() {
        match message {
            PlayMsg::Quit => {
                stop_player(&mut child);
                break;
            }
            PlayMsg::Reset => stop_player(&mut child),
            PlayMsg::Pcm(pcm) => {
                if child.is_none() {
                    child = spawn_player();
                }
                if let Some(player) = child.as_mut() {
                    if let Some(stdin) = player.stdin.as_mut() {
                        use std::io::Write;
                        if stdin.write_all(&pcm).is_err() {
                            stop_player(&mut child);
                        }
                    }
                }
            }
        }
    }
    stop_player(&mut child);
}

fn spawn_player() -> Option<std::process::Child> {
    Command::new("aplay")
        .args(["-q", "-D", "default", "-f", "S16_LE", "-r", "24000", "-c", "1", "-t", "raw"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn stop_player(child: &mut Option<std::process::Child>) {
    if let Some(mut player) = child.take() {
        let _ = player.kill();
        let _ = player.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holding_f1_toggles_once() {
        let mut held = false;
        assert!(f1_edge(&mut held, true));
        assert!(!f1_edge(&mut held, true));
        assert!(!f1_edge(&mut held, false));
        assert!(f1_edge(&mut held, true));
    }

    #[test]
    fn session_update_uses_semantic_vad_and_the_shell_tools() {
        let update = session_update("gpt-realtime-mini");
        let text = update.to_string();
        assert!(text.contains("semantic_vad"));
        assert!(text.contains("run_command"));
        assert!(text.contains("end_voice_session"));
        assert!(text.contains("far_field"));
        assert!(!text.contains("OpenAI-Beta"));
    }

    #[test]
    fn events_parse_audio_tools_and_errors() {
        assert!(matches!(parse_event(r#"{"type":"session.created"}"#), Incoming::SessionCreated));
        let audio = STANDARD.encode([1u8, 2, 3, 4]);
        match parse_event(&format!(r#"{{"type":"response.output_audio.delta","delta":"{audio}"}}"#)) {
            Incoming::Audio(pcm) => assert_eq!(pcm, vec![1, 2, 3, 4]),
            other => panic!("unexpected {other:?}"),
        }
        match parse_event(
            r#"{"type":"response.function_call_arguments.done","call_id":"c1","name":"switch_entry","arguments":"{\"entry\":\"browser\"}"}"#,
        ) {
            Incoming::Function { name, .. } => assert_eq!(name, "switch_entry"),
            other => panic!("unexpected {other:?}"),
        }
        match parse_event(r#"{"type":"error","error":{"message":"bad key"}}"#) {
            Incoming::Error(note) => assert_eq!(note, "bad key"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(redact("rejected sk-test", "sk-test"), "rejected •••");
    }

    #[test]
    fn tls_client_builds_without_a_provider_panic() {
        install_tls();
        let _config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
        let payload: Box<dyn std::any::Any + Send> = Box::new("Could not automatically determine the CryptoProvider");
        let note = panic_note(payload.as_ref());
        assert!(note.starts_with("Voice stopped"));
    }
}

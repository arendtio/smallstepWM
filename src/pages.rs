//! Firefox pages in the shell strip.
//!
//! The shell starts Firefox with its own profile and a localhost WebDriver BiDi
//! port. Top-level browsing contexts become strip entries. Frames do not.
//! The profile hides Firefox's tab strip; the address bar stays.

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{json, Value};
use smithay::reexports::calloop::channel::Sender;
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub const DEBUG_PORT: u16 = 47321;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub context: String,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    pub pages: Vec<Page>,
    pub active: Option<String>,
}

pub enum Command {
    Activate(String),
    Open {
        url: String,
        reply: tokio::sync::oneshot::Sender<String>,
    },
}

pub enum Report {
    Snapshot {
        generation: u64,
        pages: Vec<Page>,
        active: Option<String>,
    },
    Stopped {
        generation: u64,
        note: String,
    },
}

pub fn profile_dir() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
    PathBuf::from(home).join(".local/share/smallstepwm/firefox")
}

pub fn user_js() -> &'static str {
    r#"user_pref("toolkit.legacyUserProfileCustomizations.stylesheets", true);
user_pref("browser.aboutwelcome.enabled", false);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("browser.tabs.warnOnClose", false);
"#
}

pub fn user_chrome_css() -> &'static str {
    "/* Pages are listed in the shell strip. The address bar stays. */\n\
#TabsToolbar { visibility: collapse !important; }\n\
#tabbrowser-tabs { visibility: collapse !important; }\n"
}

pub fn ensure_profile(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("chrome"))?;
    std::fs::write(dir.join("user.js"), user_js())?;
    std::fs::write(dir.join("chrome").join("userChrome.css"), user_chrome_css())?;
    Ok(())
}

pub fn launch_args(profile: &Path, already_running: bool) -> Vec<std::ffi::OsString> {
    let mut args = vec![std::ffi::OsString::from("-profile"), profile.as_os_str().to_os_string()];
    if already_running {
        args.push("--new-window".into());
    } else {
        args.push("-new-instance".into());
        args.push("--remote-debugging-port".into());
        args.push(DEBUG_PORT.to_string().into());
    }
    args
}

pub fn label(title: &str, url: &str) -> String {
    let title = title.trim();
    let generic = title.is_empty()
        || title.eq_ignore_ascii_case("mozilla firefox")
        || title.eq_ignore_ascii_case("about:blank");
    let source = if generic { host_label(url) } else { title.to_string() };
    truncate_label(&source, 18)
}

fn host_label(url: &str) -> String {
    if url.is_empty() || url.starts_with("about:") {
        return "New page".into();
    }
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.trim_start_matches("www.");
    if host.is_empty() { "Page".into() } else { host.to_string() }
}

fn truncate_label(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

pub fn normalize_url(raw: &str) -> Result<String, &'static str> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2000 || raw.contains([' ', '\n', '\r']) {
        return Err("missing url");
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with("data:") || lower.starts_with("file:") {
        return Err("unsupported url");
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(raw.to_string());
    }
    if lower.contains(':') {
        return Err("unsupported url");
    }
    Ok(format!("https://{raw}"))
}

pub fn pick(pages: &[Page], query: &str) -> Option<usize> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    if let Ok(number) = query.parse::<usize>() {
        if (1..=pages.len()).contains(&number) {
            return Some(number - 1);
        }
    }
    let query = query.to_ascii_lowercase();
    pages.iter().position(|page| {
        label(&page.title, &page.url).to_ascii_lowercase().contains(&query)
            || page.url.to_ascii_lowercase().contains(&query)
    })
}

pub fn apply_event(book: &mut Book, method: &str, params: &Value) -> bool {
    let context = params.get("context").and_then(Value::as_str).unwrap_or("");
    if context.is_empty() {
        return false;
    }
    match method {
        "browsingContext.contextCreated" => {
            if params.get("parent").and_then(Value::as_str).is_some() {
                return false;
            }
            let url = params.get("url").and_then(Value::as_str).unwrap_or("").to_string();
            let existed = book.pages.iter().any(|page| page.context == context);
            let changed = upsert(book, context, url);
            if !existed {
                book.active = Some(context.to_string());
            }
            changed || !existed
        }
        "browsingContext.contextDestroyed" => {
            let before = book.pages.len();
            book.pages.retain(|page| page.context != context);
            if book.active.as_deref() == Some(context) {
                book.active = book.pages.last().map(|page| page.context.clone());
            }
            book.pages.len() != before
        }
        "browsingContext.navigationStarted"
        | "browsingContext.domContentLoaded"
        | "browsingContext.fragmentNavigated"
        | "browsingContext.historyUpdated"
        | "browsingContext.load" => {
            let changed = params
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|url| set_url(book, context, url));
            let retry_title = matches!(method, "browsingContext.domContentLoaded" | "browsingContext.load")
                && book.pages.iter().any(|page| {
                    page.context == context && page.title.is_empty() && !page.url.is_empty() && page.url != "about:blank"
                });
            changed || retry_title
        }
        _ => false,
    }
}

fn upsert(book: &mut Book, context: &str, url: String) -> bool {
    if let Some(page) = book.pages.iter_mut().find(|page| page.context == context) {
        if url.is_empty() || page.url == url {
            return false;
        }
        page.url = url;
        page.title.clear();
        return true;
    }
    book.pages.push(Page { context: context.to_string(), url, title: String::new() });
    true
}

fn set_url(book: &mut Book, context: &str, url: &str) -> bool {
    let Some(page) = book.pages.iter_mut().find(|page| page.context == context) else {
        return false;
    };
    if page.url == url {
        return false;
    }
    page.url = url.to_string();
    page.title.clear();
    true
}

pub fn replace_tree(book: &mut Book, contexts: &Value) {
    let Some(list) = contexts.as_array() else {
        return;
    };
    let mut pages = Vec::new();
    for node in list {
        if node.get("parent").and_then(Value::as_str).is_some() {
            continue;
        }
        let Some(context) = node.get("context").and_then(Value::as_str) else {
            continue;
        };
        let url = node.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        let title = book
            .pages
            .iter()
            .find(|page| page.context == context)
            .map(|page| page.title.clone())
            .unwrap_or_default();
        pages.push(Page { context: context.to_string(), url, title });
    }
    if book.active.as_ref().is_some_and(|id| !pages.iter().any(|page| &page.context == id)) {
        book.active = None;
    }
    if book.active.is_none() {
        book.active = pages.first().map(|page| page.context.clone());
    }
    book.pages = pages;
}

pub fn title_from_evaluate(result: &Value) -> Option<String> {
    let value = result.get("result")?;
    if value.get("type").and_then(Value::as_str) != Some("string") {
        return None;
    }
    value.get("value").and_then(Value::as_str).map(str::trim).map(str::to_string).filter(|title| !title.is_empty())
}

pub fn websocket_url_from_body(body: &str) -> Option<String> {
    let json_start = body.find('{')?;
    let value: Value = serde_json::from_str(&body[json_start..]).ok()?;
    let url = value.get("webSocketDebuggerUrl").and_then(Value::as_str)?;
    if url.starts_with("ws://127.0.0.1:") || url.starts_with("ws://localhost:") {
        Some(url.to_string())
    } else {
        None
    }
}

pub fn panic_note(payload: &(dyn std::any::Any + Send)) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    let detail: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let detail: String = detail.chars().take(80).collect();
    if detail.is_empty() { "Page list stopped".into() } else { format!("Page list stopped: {detail}") }
}

pub fn run(cmd_rx: tokio::sync::mpsc::UnboundedReceiver<Command>, report_tx: Sender<Report>, generation: u64) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| "page list failed".to_string())?;
    runtime.block_on(drive(cmd_rx, &report_tx, generation))
}

async fn drive(
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<Command>,
    report_tx: &Sender<Report>,
    generation: u64,
) -> Result<(), String> {
    let mut queued = Vec::new();
    let url = match wait_for_debugger(&mut cmd_rx, &mut queued).await {
        Ok(url) => url,
        Err(err) => {
            fail_open(&mut queued, &err);
            return Err(err);
        }
    };
    let (socket, _) = connect_async(url.as_str()).await.map_err(|err| clip(&err.to_string()))?;
    let (mut write, mut read) = socket.split();
    let mut book = Book::default();
    let mut next_id = 1u64;
    let mut titles: HashMap<u64, String> = HashMap::new();
    let mut opening: HashMap<u64, (String, tokio::sync::oneshot::Sender<String>)> = HashMap::new();
    let mut ready = false;
    let session_id = fresh_id(&mut next_id);
    send(&mut write, session_id, "session.new", json!({"capabilities": {}})).await?;
    let mut tree_id = 0u64;

    loop {
        tokio::select! {
            incoming = read.next() => {
                let Some(message) = incoming else {
                    fail_open_map(&mut opening, "page list closed");
                    return Ok(());
                };
                let message = message.map_err(|err| clip(&err.to_string()))?;
                let Message::Text(text) = message else { continue };
                let Ok(value) = serde_json::from_str::<Value>(text.as_ref()) else { continue };
                if value.get("type").and_then(Value::as_str) == Some("event") {
                    let method = value.get("method").and_then(Value::as_str).unwrap_or("");
                    let params = value.get("params").cloned().unwrap_or(Value::Null);
                    if apply_event(&mut book, method, &params) {
                        request_missing_titles(&mut write, &mut next_id, &mut titles, &book).await?;
                        publish(report_tx, generation, &book);
                    }
                    continue;
                }
                let Some(id) = value.get("id").and_then(Value::as_u64) else { continue };
                if value.get("type").and_then(Value::as_str) == Some("error") || value.get("error").is_some() {
                    let message = value
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
                        .unwrap_or("page list failed");
                    if let Some((_, reply)) = opening.remove(&id) {
                        let _ = reply.send(clip(message));
                    }
                    titles.remove(&id);
                    continue;
                }
                let result = value.get("result").cloned().unwrap_or(Value::Null);
                if id == session_id {
                    let subscribe_id = fresh_id(&mut next_id);
                    send(
                        &mut write,
                        subscribe_id,
                        "session.subscribe",
                        json!({
                            "events": [
                                "browsingContext.contextCreated",
                                "browsingContext.contextDestroyed",
                                "browsingContext.navigationStarted",
                                "browsingContext.domContentLoaded",
                                "browsingContext.fragmentNavigated",
                                "browsingContext.historyUpdated"
                            ]
                        }),
                    )
                    .await?;
                    tree_id = fresh_id(&mut next_id);
                    send(&mut write, tree_id, "browsingContext.getTree", json!({"maxDepth": 0})).await?;
                } else if id == tree_id {
                    if let Some(contexts) = result.get("contexts") {
                        replace_tree(&mut book, contexts);
                        request_missing_titles(&mut write, &mut next_id, &mut titles, &book).await?;
                        publish(report_tx, generation, &book);
                    }
                    ready = true;
                    let pending = std::mem::take(&mut queued);
                    for command in pending {
                        dispatch(&mut write, &mut next_id, &mut book, &mut opening, report_tx, generation, command).await?;
                    }
                } else if let Some(context) = titles.remove(&id) {
                    if let Some(title) = title_from_evaluate(&result) {
                        if let Some(page) = book.pages.iter_mut().find(|page| page.context == context) {
                            page.title = title;
                            publish(report_tx, generation, &book);
                        }
                    }
                } else if let Some((url, reply)) = opening.remove(&id) {
                    let Some(context) = result.get("context").and_then(Value::as_str) else {
                        let _ = reply.send("could not open the page".into());
                        continue;
                    };
                    let context = context.to_string();
                    book.active = Some(context.clone());
                    upsert(&mut book, &context, url.clone());
                    publish(report_tx, generation, &book);
                    let nav_id = fresh_id(&mut next_id);
                    send(
                        &mut write,
                        nav_id,
                        "browsingContext.navigate",
                        json!({"context": context, "url": url, "wait": "none"}),
                    )
                    .await?;
                    let _ = reply.send("opened".into());
                }
            }
            command = cmd_rx.recv() => {
                let Some(command) = command else {
                    fail_open_map(&mut opening, "page list closed");
                    return Ok(());
                };
                if ready {
                    dispatch(&mut write, &mut next_id, &mut book, &mut opening, report_tx, generation, command).await?;
                } else {
                    queued.push(command);
                }
            }
        }
    }
}

async fn wait_for_debugger(
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    queued: &mut Vec<Command>,
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if Instant::now() >= deadline {
            return Err("page list failed".into());
        }
        if let Some(url) = debugger_url(DEBUG_PORT) {
            return Ok(url);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            command = cmd_rx.recv() => {
                match command {
                    Some(command) => queued.push(command),
                    None => return Err("page list closed".into()),
                }
            }
        }
    }
}

fn debugger_url(port: u16) -> Option<String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(400)).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(400))).ok()?;
    stream
        .write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    websocket_url_from_body(&body)
}

fn fresh_id(next: &mut u64) -> u64 {
    let id = *next;
    *next += 1;
    id
}

async fn send<S>(write: &mut S, id: u64, method: &str, params: Value) -> Result<(), String>
where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let text = json!({"id": id, "method": method, "params": params}).to_string();
    write.send(Message::Text(text.into())).await.map_err(|err| clip(&err.to_string()))
}

async fn request_missing_titles<S>(
    write: &mut S,
    next_id: &mut u64,
    titles: &mut HashMap<u64, String>,
    book: &Book,
) -> Result<(), String>
where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    if titles.len() >= 4 {
        return Ok(());
    }
    for page in &book.pages {
        if !page.title.is_empty() || titles.values().any(|context| context == &page.context) {
            continue;
        }
        if page.url.is_empty() || page.url == "about:blank" {
            continue;
        }
        let id = fresh_id(next_id);
        titles.insert(id, page.context.clone());
        send(
            write,
            id,
            "script.evaluate",
            json!({
                "expression": "document.title",
                "target": { "context": page.context },
                "awaitPromise": false,
                "resultOwnership": "none"
            }),
        )
        .await?;
        if titles.len() >= 4 {
            break;
        }
    }
    Ok(())
}

async fn dispatch<S>(
    write: &mut S,
    next_id: &mut u64,
    book: &mut Book,
    opening: &mut HashMap<u64, (String, tokio::sync::oneshot::Sender<String>)>,
    report_tx: &Sender<Report>,
    generation: u64,
    command: Command,
) -> Result<(), String>
where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match command {
        Command::Activate(context) => {
            if book.pages.iter().any(|page| page.context == context) {
                book.active = Some(context.clone());
                publish(report_tx, generation, book);
            }
            let id = fresh_id(next_id);
            send(write, id, "browsingContext.activate", json!({"context": context})).await?;
        }
        Command::Open { url, reply } => {
            let id = fresh_id(next_id);
            opening.insert(id, (url, reply));
            send(write, id, "browsingContext.create", json!({"type": "tab"})).await?;
        }
    }
    Ok(())
}

fn publish(report_tx: &Sender<Report>, generation: u64, book: &Book) {
    let _ = report_tx.send(Report::Snapshot {
        generation,
        pages: book.pages.clone(),
        active: book.active.clone(),
    });
}

fn fail_open(queued: &mut Vec<Command>, note: &str) {
    for command in queued.drain(..) {
        if let Command::Open { reply, .. } = command {
            let _ = reply.send(note.to_string());
        }
    }
}

fn fail_open_map(opening: &mut HashMap<u64, (String, tokio::sync::oneshot::Sender<String>)>, note: &str) {
    for (_, (_, reply)) in opening.drain() {
        let _ = reply.send(note.to_string());
    }
}

fn clip(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_pages_are_kept_and_frames_are_not() {
        let mut book = Book::default();
        assert!(apply_event(
            &mut book,
            "browsingContext.contextCreated",
            &json!({"context": "tab", "parent": null, "url": "https://example.com/"})
        ));
        assert!(!apply_event(
            &mut book,
            "browsingContext.contextCreated",
            &json!({"context": "frame", "parent": "tab", "url": "https://ads.example/"})
        ));
        assert_eq!(book.pages.len(), 1);
        assert_eq!(book.active.as_deref(), Some("tab"));
        apply_event(
            &mut book,
            "browsingContext.contextCreated",
            &json!({"context": "second", "parent": null, "url": "about:blank"}),
        );
        assert_eq!(book.active.as_deref(), Some("second"));
        assert!(apply_event(
            &mut book,
            "browsingContext.navigationStarted",
            &json!({"context": "tab", "url": "https://example.com/next"})
        ));
        assert_eq!(book.pages[0].url, "https://example.com/next");
        assert_eq!(book.pages[0].title, "");
        replace_tree(
            &mut book,
            &json!([
                {"context": "tab", "parent": null, "url": "https://example.com/next", "children": [
                    {"context": "frame", "url": "https://ads.example/"}
                ]},
                {"context": "second", "parent": null, "url": "https://en.wikipedia.org/"}
            ]),
        );
        assert_eq!(book.pages.len(), 2);
        assert!(title_from_evaluate(&json!({"result": {"type": "string", "value": "Example"}})).as_deref() == Some("Example"));
        apply_event(&mut book, "browsingContext.contextDestroyed", &json!({"context": "second"}));
        assert_eq!(book.active.as_deref(), Some("tab"));
    }

    #[test]
    fn labels_urls_and_launch_args() {
        assert_eq!(label("Mozilla Firefox", "https://www.example.com/a"), "example.com");
        assert_eq!(label("Ein Titel mit Umlaut ä", "https://example.com/"), "Ein Titel mit U...");
        assert_eq!(normalize_url("example.com/path").as_deref(), Ok("https://example.com/path"));
        assert!(normalize_url("javascript:alert(1)").is_err());
        assert!(normalize_url("file:///etc/passwd").is_err());
        let pages = vec![Page { context: "a".into(), url: "https://en.wikipedia.org/".into(), title: "Wikipedia".into() }];
        assert_eq!(pick(&pages, "1"), Some(0));
        assert_eq!(pick(&pages, "wiki"), Some(0));
        assert_eq!(pick(&pages, "missing"), None);
        let profile = Path::new("/tmp/smallstep-profile");
        let first = launch_args(profile, false);
        assert!(first.iter().any(|arg| arg == "--remote-debugging-port"));
        assert!(first.iter().any(|arg| arg == "-new-instance"));
        let again = launch_args(profile, true);
        assert!(again.iter().any(|arg| arg == "--new-window"));
        assert!(!again.iter().any(|arg| arg == "--remote-debugging-port"));
        let body = "HTTP/1.1 200 OK\r\n\r\n{\"webSocketDebuggerUrl\":\"ws://127.0.0.1:47321/session\"}";
        assert_eq!(websocket_url_from_body(body).as_deref(), Some("ws://127.0.0.1:47321/session"));
        assert!(websocket_url_from_body("{\"webSocketDebuggerUrl\":\"ws://example.com/session\"}").is_none());
        assert!(user_chrome_css().contains("#TabsToolbar"));
        let dir = std::env::temp_dir().join(format!("smallstep-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_profile(&dir).unwrap();
        assert!(dir.join("chrome").join("userChrome.css").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

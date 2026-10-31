//! Trang chat trên máy (127.0.0.1) để hiển thị trong OBS và phát tiếng từ trình duyệt.
use crate::hub::Hub;
use crate::player::READING_PREFIX;
use crate::runner::{Logger, Runner};
use crate::tts::Tts;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX_HTML: &str = include_str!("../index.html");
const MAX_BODY: usize = 4096;
const LONG_POLL: Duration = Duration::from_secs(25);
const PING: Duration = Duration::from_secs(15);

pub struct Shared {
    pub hub: Arc<Hub>,
    pub runner: Arc<Runner>,
    pub tts: Arc<Mutex<Tts>>,
    pub port: u16,
    pub log: Logger,
    /// Báo cho giao diện khi link được đổi từ trang chat.
    pub on_target_changed: Box<dyn Fn(String) + Send + Sync>,
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
}

fn respond(request: Request, status: u16, content_type: &str, body: Vec<u8>) {
    let response = Response::from_data(body)
        .with_status_code(status)
        .with_header(header("Content-Type", content_type))
        .with_header(header("Cache-Control", "no-store"));
    let _ = request.respond(response);
}

fn respond_json(request: Request, status: u16, data: Value) {
    respond(request, status, "application/json; charset=utf-8", data.to_string().into_bytes());
}

fn events(request: Request, hub: &Hub) {
    let receiver = hub.subscribe();
    let mut writer = request.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    if writer.write_all(head.as_bytes()).is_err() || writer.flush().is_err() {
        return;
    }
    loop {
        let chunk = match receiver.recv_timeout(PING) {
            Ok(event) => format!("data: {event}\n\n"),
            Err(RecvTimeoutError::Timeout) => ": ping\n\n".to_string(),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Trang đã đóng thì lệnh ghi báo lỗi
        if writer.write_all(chunk.as_bytes()).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

fn next_speech(request: Request, shared: &Shared) {
    let audio = shared.hub.next_speech(LONG_POLL).and_then(|text| {
        let audio = shared.tts.lock().unwrap().synthesize(&text);
        match audio {
            Ok(audio) => {
                (shared.log)(format!("{READING_PREFIX}{text}"));
                Some(audio)
            }
            Err(error) => {
                (shared.log)(format!("Bỏ qua một bình luận vì không tạo được giọng đọc: {error}"));
                None
            }
        }
    });
    match audio {
        Some(audio) => {
            // VieNeu trả WAV, dịch vụ Microsoft trả MP3
            let content_type = if audio.starts_with(b"RIFF") { "audio/wav" } else { "audio/mpeg" };
            respond(request, 200, content_type, audio);
        }
        None => respond(request, 204, "text/plain", Vec::new()),
    }
}

fn set_target(mut request: Request, shared: &Shared) {
    // Không cho trang web khác đổi link qua cổng localhost này
    let origin = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Origin"))
        .map(|h| h.value.as_str().to_string());
    let allowed = [
        format!("http://127.0.0.1:{}", shared.port),
        format!("http://localhost:{}", shared.port),
    ];
    if !origin.is_some_and(|origin| allowed.contains(&origin)) {
        respond_json(request, 403, json!({ "error": "Không được phép" }));
        return;
    }

    if request.body_length().unwrap_or(0) > MAX_BODY {
        respond_json(request, 413, json!({ "error": "Link quá dài" }));
        return;
    }
    let mut body = String::new();
    let read = request.as_reader().take(MAX_BODY as u64).read_to_string(&mut body);
    let target = read
        .ok()
        .and_then(|_| serde_json::from_str::<Value>(&body).ok())
        .and_then(|data| data.get("target")?.as_str().map(String::from));
    let Some(target) = target else {
        respond_json(request, 400, json!({ "error": "Link không hợp lệ" }));
        return;
    };
    match shared.runner.set_target(&target) {
        Ok(()) => {
            (shared.on_target_changed)(target.trim().to_string());
            respond_json(request, 200, json!({ "target": shared.hub.target() }));
        }
        Err(error) => respond_json(request, 400, json!({ "error": error })),
    }
}

fn handle(request: Request, shared: &Shared) {
    let path = request.url().split('?').next().unwrap_or("").to_string();
    let method = request.method().clone();
    match (method, path.as_str()) {
        (Method::Get, "/") => respond(request, 200, "text/html; charset=utf-8", INDEX_HTML.as_bytes().to_vec()),
        (Method::Get, "/events") => events(request, &shared.hub),
        (Method::Get, "/speech/next") => next_speech(request, shared),
        (Method::Post, "/target") => set_target(request, shared),
        _ => respond(request, 404, "text/plain", b"Not found".to_vec()),
    }
}

pub fn start(shared: Shared) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", shared.port)).map_err(|e| e.to_string())?;
    let shared = Arc::new(shared);
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let shared = shared.clone();
            std::thread::spawn(move || handle(request, &shared));
        }
    });
    Ok(())
}

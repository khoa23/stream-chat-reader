//! Tạo giọng đọc bằng nguồn người dùng chọn: dịch vụ Microsoft Edge (qua WebSocket) hoặc VieNeu trên máy.
use crate::edge_token::generate_sec_ms_gec;
use crate::runner::Logger;
use crate::vieneu::{Model, Worker};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::Request;
use tungstenite::http::header;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const HOST: &str = "speech.platform.bing.com";
const WSS_URL: &str = "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken=6A5AA1D4EAFF4E9FB37E23D68491D6F4&ConnectionId=";
const SEC_MS_GEC_VERSION: &str = "2-155.0.4283.45";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36 Edg/155.0.0.0";
const ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";
const AUDIO_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

const ATTEMPTS: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_millis(500);
const READ_TIMEOUT: Duration = Duration::from_secs(20);
// Kết nối nghỉ lâu hơn mức này có thể đã bị dịch vụ đóng, nên mở kết nối mới
const MAX_IDLE: Duration = Duration::from_secs(20);

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

// Lỗi khởi động VieNeu thường không tự hết, nên không thử lại với từng bình luận
const VIENEU_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Các nguồn đọc hiển thị trên giao diện.
#[derive(Clone, Copy, PartialEq)]
pub enum Engine {
    Edge,
    VieNeu(Model),
}

impl Engine {
    pub const ALL: [Engine; 3] = [Engine::Edge, Engine::VieNeu(Model::Nano), Engine::VieNeu(Model::Turbo)];

    pub fn label(self) -> &'static str {
        match self {
            Engine::Edge => "Microsoft Edge (online, nhẹ máy nhất)",
            Engine::VieNeu(Model::Nano) => "VieNeu Nano (offline, ~400 MB RAM)",
            Engine::VieNeu(Model::Turbo) => "VieNeu Turbo (offline, giọng tốt hơn, ~1 GB RAM)",
        }
    }

    /// Tên lưu trong panel.json.
    pub fn key(self) -> &'static str {
        match self {
            Engine::Edge => "edge",
            Engine::VieNeu(Model::Nano) => "vieneu-nano",
            Engine::VieNeu(Model::Turbo) => "vieneu-turbo",
        }
    }
}

/// Nguồn đọc đang chọn; giao diện đổi được ngay cả khi đang tạo giọng đọc.
pub type EngineChoice = Arc<AtomicUsize>;

pub struct Tts {
    voice: String,
    vieneu_voice: String,
    choice: EngineChoice,
    log: Logger,
    socket: Option<Socket>,
    last_used: Instant,
    worker: Option<Worker>,
    vieneu_failure: Option<(Instant, String)>,
}

fn new_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

fn build_request() -> Result<Request, String> {
    let url = format!(
        "{WSS_URL}{}&Sec-MS-GEC={}&Sec-MS-GEC-Version={SEC_MS_GEC_VERSION}",
        new_id(),
        generate_sec_ms_gec()
    );
    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
    let headers = request.headers_mut();
    headers.insert(header::PRAGMA, "no-cache".parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    headers.insert(header::USER_AGENT, USER_AGENT.parse().unwrap());
    headers.insert(header::ORIGIN, ORIGIN.parse().unwrap());
    Ok(request)
}

fn connect() -> Result<Socket, String> {
    let stream = TcpStream::connect((HOST, 443)).map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(READ_TIMEOUT)).map_err(|e| e.to_string())?;
    let (socket, _) = tungstenite::client_tls(build_request()?, stream).map_err(|e| e.to_string())?;
    Ok(socket)
}

/// Thoát ký tự XML và bỏ ký tự điều khiển mà dịch vụ không nhận.
fn escape_ssml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn synthesize_once(socket: &mut Socket, text: &str, voice: &str) -> Result<Vec<u8>, String> {
    let timestamp = httpdate::fmt_http_date(SystemTime::now());
    let config = format!(
        "X-Timestamp:{timestamp}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n\
{{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"{AUDIO_FORMAT}\"}}}}}}}}"
    );
    let ssml = format!(
        "X-RequestId:{}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{timestamp}\r\nPath:ssml\r\n\r\n\
<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'><voice name='{voice}'><prosody pitch='+0Hz' rate='+0%' volume='+0%'>{}</prosody></voice></speak>",
        new_id(),
        escape_ssml(text)
    );
    socket.send(Message::text(config)).map_err(|e| e.to_string())?;
    socket.send(Message::text(ssml)).map_err(|e| e.to_string())?;

    let mut audio = Vec::new();
    loop {
        match socket.read().map_err(|e| e.to_string())? {
            Message::Text(message) if message.contains("Path:turn.end") => break,
            Message::Binary(frame) if frame.len() >= 2 => {
                let header_end = 2 + u16::from_be_bytes([frame[0], frame[1]]) as usize;
                if header_end <= frame.len() {
                    audio.extend_from_slice(&frame[header_end..]);
                }
            }
            Message::Close(_) => return Err("dịch vụ đóng kết nối".to_string()),
            _ => {}
        }
    }
    if audio.is_empty() {
        return Err("dịch vụ không trả về âm thanh".to_string());
    }
    Ok(audio)
}

impl Tts {
    pub fn new(voice: String, vieneu_voice: String, choice: EngineChoice, log: Logger) -> Self {
        Tts {
            voice,
            vieneu_voice,
            choice,
            log,
            socket: None,
            last_used: Instant::now(),
            worker: None,
            vieneu_failure: None,
        }
    }

    fn engine(&self) -> Engine {
        Engine::ALL[self.choice.load(Ordering::Relaxed) % Engine::ALL.len()]
    }

    /// Không tự chuyển nguồn khi lỗi: người dùng chọn offline thì bình luận không được gửi ra ngoài.
    pub fn synthesize(&mut self, text: &str) -> Result<Vec<u8>, String> {
        match self.engine() {
            Engine::Edge => {
                self.worker = None;
                self.synthesize_edge(text)
            }
            Engine::VieNeu(model) => {
                self.socket = None;
                self.synthesize_vieneu(model, text)
            }
        }
    }

    /// Tắt VieNeu để trả RAM (khi đã dừng đọc).
    pub fn release_worker(&mut self) {
        self.worker = None;
        self.vieneu_failure = None;
    }

    /// Tắt VieNeu nếu người dùng đã chuyển sang nguồn khác.
    pub fn drop_unused_worker(&mut self) {
        let engine = self.engine();
        if self.worker.as_ref().is_some_and(|worker| engine != Engine::VieNeu(worker.model())) {
            self.worker = None;
        }
    }

    fn synthesize_vieneu(&mut self, model: Model, text: &str) -> Result<Vec<u8>, String> {
        if self.worker.as_ref().is_some_and(|worker| worker.model() != model) {
            self.worker = None;
            self.vieneu_failure = None;
        }
        if self.worker.is_none() {
            if let Some((at, error)) = &self.vieneu_failure {
                if at.elapsed() < VIENEU_RETRY_DELAY {
                    return Err(error.clone());
                }
            }
            match Worker::start(model, &self.vieneu_voice, &self.log) {
                Ok(worker) => {
                    self.worker = Some(worker);
                    self.vieneu_failure = None;
                }
                Err(error) => {
                    let error = format!("{} chưa chạy được: {error}", model.label());
                    self.vieneu_failure = Some((Instant::now(), error.clone()));
                    return Err(error);
                }
            }
        }
        let worker = self.worker.as_mut().expect("vừa khởi động ở trên");
        worker.synthesize(text).map_err(|(error, alive)| {
            if !alive {
                self.worker = None;
            }
            error
        })
    }

    /// Dịch vụ thỉnh thoảng trả lỗi hoặc không có âm thanh với câu bình thường, nên thử lại.
    fn synthesize_edge(&mut self, text: &str) -> Result<Vec<u8>, String> {
        if self.last_used.elapsed() > MAX_IDLE {
            self.socket = None;
        }
        let mut last_error = String::new();
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                sleep(RETRY_DELAY);
            }
            let result = match self.socket.take().map_or_else(connect, Ok) {
                Ok(mut socket) => synthesize_once(&mut socket, text, &self.voice).map(|audio| {
                    self.socket = Some(socket);
                    audio
                }),
                Err(error) => Err(error),
            };
            match result {
                Ok(audio) => {
                    self.last_used = Instant::now();
                    return Ok(audio);
                }
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssml_special_characters_are_escaped() {
        assert_eq!(escape_ssml("<b>a&b</b>"), "&lt;b&gt;a&amp;b&lt;/b&gt;");
        assert_eq!(escape_ssml("a\u{1}b"), "a b");
    }
}

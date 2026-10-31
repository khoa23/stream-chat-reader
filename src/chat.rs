//! Lấy live chat YouTube qua endpoint nội bộ của trang web (không cần API key).
use serde_json::{json, Value};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

const BASE: &str = "https://www.youtube.com";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const ACCEPT_LANGUAGE: &str = "vi,en;q=0.9";
// Bỏ qua trang hỏi đồng ý cookie
const COOKIE: &str = "CONSENT=YES+1; SOCS=CAI";
const TIMEOUT: Duration = Duration::from_secs(20);
const VIDEO_ID_LEN: usize = 11;

pub enum ChatError {
    /// Kênh/video hiện không có live chat, hoặc chat đã kết thúc.
    NotLive(String),
    Network(String),
}

pub struct Message {
    pub id: String,
    pub author: String,
    pub text: String,
    pub speech: String,
    pub amount: Option<String>,
    pub timestamp: f64,
}

/// Tín hiệu dừng cho luồng đọc chat, có thể chờ và bị đánh thức ngay khi dừng.
#[derive(Default)]
pub struct StopSignal {
    stopped: Mutex<bool>,
    changed: Condvar,
}

impl StopSignal {
    pub fn stop(&self) {
        *self.stopped.lock().unwrap() = true;
        self.changed.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        *self.stopped.lock().unwrap()
    }

    /// Chờ tối đa `delay`; trả về true nếu đã bị dừng.
    pub fn wait(&self, delay: Duration) -> bool {
        let stopped = self.stopped.lock().unwrap();
        let (stopped, _) = self
            .changed
            .wait_timeout_while(stopped, delay, |stopped| !*stopped)
            .unwrap();
        *stopped
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(TIMEOUT)
            .tls_connector(Arc::new(native_tls::TlsConnector::new().expect("Không khởi tạo được TLS")))
            .build()
    })
}

fn network_error(error: impl ToString) -> ChatError {
    ChatError::Network(error.to_string())
}

fn get(url: &str) -> Result<String, ChatError> {
    agent()
        .get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept-Language", ACCEPT_LANGUAGE)
        .set("Cookie", COOKIE)
        .call()
        .map_err(network_error)?
        .into_string()
        .map_err(network_error)
}

fn post(url: &str, payload: &Value) -> Result<Value, ChatError> {
    let body = agent()
        .post(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept-Language", ACCEPT_LANGUAGE)
        .set("Cookie", COOKIE)
        .set("Content-Type", "application/json")
        .send_string(&payload.to_string())
        .map_err(network_error)?
        .into_string()
        .map_err(network_error)?;
    serde_json::from_str(&body).map_err(network_error)
}

/// Phần văn bản ngay sau `key` và dấu phân cách (`=` hoặc `:`) của một phép gán trong trang.
fn after_key<'a>(html: &'a str, key: &'a str, separator: char) -> impl Iterator<Item = &'a str> {
    let key_len = key.len();
    html.match_indices(key).filter_map(move |(at, _)| {
        html[at + key_len..]
            .trim_start_matches(|c: char| c == '"' || c == ']' || c.is_whitespace())
            .strip_prefix(separator)
            .map(str::trim_start)
    })
}

fn json_after(html: &str, key: &str, separator: char) -> Option<Value> {
    let start = after_key(html, key, separator).find(|rest| rest.starts_with('{'))?;
    serde_json::Deserializer::from_str(start).into_iter::<Value>().next()?.ok()
}

fn string_after(html: &str, key: &str) -> Option<String> {
    let start = after_key(html, key, ':').find_map(|rest| rest.strip_prefix('"'))?;
    start.split('"').next().map(String::from)
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/@-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub enum Target {
    Video(String),
    Channel(String),
}

fn is_video_id(text: &str) -> bool {
    text.len() == VIDEO_ID_LEN
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// ID video nằm ngay sau một trong các dấu hiệu trong link.
fn video_id_in(target: &str) -> Option<&str> {
    ["?v=", "&v=", "youtu.be/", "/live/"].iter().find_map(|marker| {
        let start = target.find(marker)? + marker.len();
        target.get(start..start + VIDEO_ID_LEN).filter(|id| is_video_id(id))
    })
}

fn is_youtube_host(host: &str) -> bool {
    let is_label = |label: &str| {
        !label.is_empty() && label.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    };
    host == "youtube.com"
        || host == "youtu.be"
        || host.strip_suffix(".youtube.com").is_some_and(is_label)
        || host.strip_suffix(".youtu.be").is_some_and(is_label)
}

/// Nhận link video, ID video, @handle hoặc link kênh.
pub fn parse_target(target: &str) -> Result<Target, String> {
    let target = target.trim();
    let invalid = || format!("Không nhận ra link YouTube: {target}");

    if is_video_id(target) {
        return Ok(Target::Video(target.to_string()));
    }
    if let Some(handle) = target.strip_prefix('@') {
        let is_handle_char = |c: char| c.is_alphanumeric() || "_.-".contains(c);
        if !handle.is_empty() && handle.chars().all(is_handle_char) {
            return Ok(Target::Channel(format!("{BASE}/{}", percent_encode_path(target))));
        }
        return Err(invalid());
    }

    if target.contains(char::is_whitespace) {
        return Err(invalid());
    }
    let without_scheme = target
        .strip_prefix("https://")
        .or_else(|| target.strip_prefix("http://"))
        .unwrap_or(target);
    let (host, path) = without_scheme
        .find('/')
        .map(|slash| without_scheme.split_at(slash))
        .ok_or_else(invalid)?;
    if !is_youtube_host(host) {
        return Err(invalid());
    }
    if let Some(id) = video_id_in(target) {
        return Ok(Target::Video(id.to_string()));
    }
    let path = path.split('?').next().unwrap_or("").trim_end_matches('/');
    if !host.ends_with("youtube.com") || path.is_empty() {
        return Err(invalid());
    }
    Ok(Target::Channel(format!("{BASE}{}", percent_encode_path(path))))
}

/// Trả về ID video đang live của link đã nhập.
pub fn resolve_video_id(target: &str) -> Result<String, ChatError> {
    let channel = match parse_target(target).map_err(ChatError::Network)? {
        Target::Video(id) => return Ok(id),
        Target::Channel(url) => url,
    };
    let html = get(&format!("{channel}/live"))?;
    let not_live = || ChatError::NotLive(format!("{target} hiện không livestream"));
    let (_, canonical) = html.split_once(r#"<link rel="canonical" href=""#).ok_or_else(not_live)?;
    let url = canonical.split('"').next().unwrap_or("");
    url.split_once("watch?v=")
        .map(|(_, id)| id)
        .filter(|id| is_video_id(id))
        .map(String::from)
        .ok_or_else(not_live)
}

fn str_at(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer)?.as_str().map(String::from)
}

fn parse_action(action: &Value) -> Option<Message> {
    let item = action.pointer("/addChatItemAction/item")?;
    let renderer = item
        .get("liveChatTextMessageRenderer")
        .or_else(|| item.get("liveChatPaidMessageRenderer"))?;

    let mut text = String::new();
    let mut speech = String::new();
    for run in renderer.pointer("/message/runs")?.as_array()? {
        if let Some(part) = run.get("text").and_then(Value::as_str) {
            text.push_str(part);
            speech.push_str(part);
        } else if let Some(emoji) = run.get("emoji") {
            let is_custom = emoji.get("isCustomEmoji").and_then(Value::as_bool).unwrap_or(false);
            match str_at(emoji, "/shortcuts/0") {
                Some(shortcut) if is_custom => text.push_str(&shortcut),
                _ => text.push_str(&str_at(emoji, "/emojiId").unwrap_or_default()),
            }
        }
    }

    Some(Message {
        id: str_at(renderer, "/id")?,
        author: str_at(renderer, "/authorName/simpleText").unwrap_or_default(),
        text,
        speech,
        amount: str_at(renderer, "/purchaseAmountText/simpleText"),
        timestamp: str_at(renderer, "/timestampUsec")
            .and_then(|usec| usec.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1_000_000.0,
    })
}

fn each_message(actions: Option<&Value>, on_message: &mut impl FnMut(Message)) {
    for action in actions.and_then(Value::as_array).into_iter().flatten() {
        if let Some(message) = parse_action(action) {
            on_message(message);
        }
    }
}

fn first_continuation(continuations: &Value) -> Option<&Value> {
    continuations.get(0)?.as_object()?.values().next()
}

/// Chọn chế độ "Trò chuyện trực tiếp" (mọi tin nhắn) thay vì "Tin nhắn hàng đầu".
fn all_chat_continuation(renderer: &Value) -> Option<String> {
    let items = renderer
        .pointer("/header/liveChatHeaderRenderer/viewSelector/sortFilterSubMenuRenderer/subMenuItems")
        .and_then(Value::as_array);
    items
        .and_then(|items| items.last())
        .and_then(|item| str_at(item, "/continuation/reloadContinuationData/continuation"))
        .or_else(|| str_at(first_continuation(renderer.get("continuations")?)?, "/continuation"))
}

/// Gọi `on_message` với từng tin nhắn cho tới khi bị dừng (Ok) hoặc chat kết thúc (Err).
pub fn stream(
    video_id: &str,
    stop: &StopSignal,
    mut on_message: impl FnMut(Message),
) -> Result<(), ChatError> {
    let no_chat = || ChatError::NotLive(format!("Video {video_id} không có live chat"));

    // Trang chat nặng cỡ MB: chỉ lấy phần cần dùng rồi giải phóng trước khi đọc chat lâu dài
    let (key, context, mut continuation) = {
        let html = get(&format!("{BASE}/live_chat?is_popout=1&v={video_id}"))?;
        let data = json_after(&html, "ytInitialData", '=').ok_or_else(no_chat)?;
        let renderer = data.pointer("/contents/liveChatRenderer").ok_or_else(no_chat)?;
        each_message(renderer.get("actions"), &mut on_message);
        (
            string_after(&html, "\"INNERTUBE_API_KEY\"").ok_or_else(no_chat)?,
            json_after(&html, "\"INNERTUBE_CONTEXT\"", ':').ok_or_else(no_chat)?,
            all_chat_continuation(renderer).ok_or_else(no_chat)?,
        )
    };

    let url = format!("{BASE}/youtubei/v1/live_chat/get_live_chat?key={key}");
    loop {
        let response = post(&url, &json!({ "context": context, "continuation": continuation }))?;
        let ended = || ChatError::NotLive(format!("Live chat của video {video_id} đã kết thúc"));
        let chat = response
            .pointer("/continuationContents/liveChatContinuation")
            .ok_or_else(ended)?;
        each_message(chat.get("actions"), &mut on_message);

        let next = chat
            .get("continuations")
            .and_then(first_continuation)
            .ok_or_else(ended)?;
        continuation = str_at(next, "/continuation").ok_or_else(ended)?;
        let timeout_ms = next.get("timeoutMs").and_then(Value::as_u64).unwrap_or(3000);
        if stop.wait(Duration::from_millis(timeout_ms.clamp(1000, 10_000))) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(target: &str) -> Option<String> {
        match parse_target(target) {
            Ok(Target::Video(id)) => Some(id),
            _ => None,
        }
    }

    fn channel(target: &str) -> Option<String> {
        match parse_target(target) {
            Ok(Target::Channel(url)) => Some(url),
            _ => None,
        }
    }

    #[test]
    fn video_links_are_recognized() {
        let id = Some("gCNeDWCI0vo".to_string());
        assert_eq!(video("gCNeDWCI0vo"), id);
        assert_eq!(video("https://www.youtube.com/watch?v=gCNeDWCI0vo"), id);
        assert_eq!(video("https://www.youtube.com/watch?si=x&v=gCNeDWCI0vo&t=1"), id);
        assert_eq!(video("youtu.be/gCNeDWCI0vo"), id);
        assert_eq!(video("https://www.youtube.com/live/gCNeDWCI0vo?si=x"), id);
        assert_eq!(video("  https://m.youtube.com/watch?v=gCNeDWCI0vo  "), id);
    }

    #[test]
    fn channel_links_are_recognized() {
        let lofi = Some("https://www.youtube.com/@LofiGirl".to_string());
        assert_eq!(channel("@LofiGirl"), lofi);
        assert_eq!(channel("https://youtube.com/@LofiGirl"), lofi);
        assert_eq!(channel("https://www.youtube.com/@LofiGirl/?x=1"), lofi);
        assert_eq!(
            channel("https://m.youtube.com/channel/UCabc/"),
            Some("https://www.youtube.com/channel/UCabc".to_string())
        );
        assert_eq!(channel("@tên"), Some("https://www.youtube.com/@t%C3%AAn".to_string()));
    }

    #[test]
    fn other_links_are_rejected() {
        for target in [
            "https://evil.com/x",
            "https://evil.com/youtube.com/x",
            "https://youtube.com.evil.com/@x",
            "https://evil.com/?v=gCNeDWCI0vo",
            "https://notyoutube.com/@x",
            "https://a.b.youtube.com/@x",
            "hello",
            "",
            "@",
            "obs",
            "https://youtu.be/",
            "https://youtube.com/",
            "youtube.com",
            "https://youtube.com/@a b",
        ] {
            assert!(parse_target(target).is_err(), "{target}");
        }
    }

    #[test]
    fn page_values_are_found_after_their_assignment() {
        let html = r#"x "ytInitialData" y; window["ytInitialData"] = {"a":1}; z"#;
        assert_eq!(json_after(html, "ytInitialData", '='), Some(json!({"a": 1})));
        let html = r#"{"INNERTUBE_API_KEY" : "abc", "INNERTUBE_CONTEXT":{"b":2}, "c":3}"#;
        assert_eq!(string_after(html, "\"INNERTUBE_API_KEY\"").as_deref(), Some("abc"));
        assert_eq!(json_after(html, "\"INNERTUBE_CONTEXT\"", ':'), Some(json!({"b": 2})));
        assert_eq!(json_after("nothing", "ytInitialData", '='), None);
    }
}

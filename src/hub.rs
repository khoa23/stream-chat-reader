//! Chuyển tin nhắn từ luồng đọc chat sang các trang đang mở và hàng đợi đọc to.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const NO_TARGET_STATUS: &str = "Chưa có link. Dán link livestream vào ô phía trên.";
pub const MAX_SPEECH_AGE: f64 = 120.0; // giây; chỉ đọc bình luận được gửi trong khoảng thời gian này
const MAX_RECENT: usize = 50;
const MAX_SPEECH_QUEUE: usize = 20;

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

struct State {
    recent: VecDeque<String>,
    subscribers: Vec<Sender<String>>,
    speech: VecDeque<(f64, String)>,
    target: String,
    status: String,
}

pub struct Hub {
    state: Mutex<State>,
    speech_ready: Condvar,
}

fn clear_event(target: &str) -> String {
    json!({ "type": "clear", "target": target }).to_string()
}

fn status_event(text: &str) -> String {
    json!({ "type": "status", "text": text }).to_string()
}

fn send_to_all(state: &mut State, event: &str) {
    // Trang đã đóng thì kênh của nó báo lỗi và bị loại khỏi danh sách
    state.subscribers.retain(|s| s.send(event.to_string()).is_ok());
}

impl Hub {
    pub fn new() -> Self {
        Hub {
            state: Mutex::new(State {
                recent: VecDeque::new(),
                subscribers: Vec::new(),
                speech: VecDeque::new(),
                target: String::new(),
                status: NO_TARGET_STATUS.to_string(),
            }),
            speech_ready: Condvar::new(),
        }
    }

    pub fn publish_chat(&self, event: &Value) {
        let event = event.to_string();
        let mut state = self.state.lock().unwrap();
        if state.recent.len() == MAX_RECENT {
            state.recent.pop_front();
        }
        state.recent.push_back(event.clone());
        send_to_all(&mut state, &event);
    }

    pub fn publish_status(&self, text: &str) {
        let mut state = self.state.lock().unwrap();
        state.status = text.to_string();
        send_to_all(&mut state, &status_event(text));
    }

    pub fn subscribe(&self) -> Receiver<String> {
        let (sender, receiver) = channel();
        let mut state = self.state.lock().unwrap();
        // "clear" trước để trang tự kết nối lại không bị lặp tin nhắn
        let _ = sender.send(clear_event(&state.target));
        let _ = sender.send(status_event(&state.status));
        for event in &state.recent {
            let _ = sender.send(event.clone());
        }
        state.subscribers.push(sender);
        receiver
    }

    /// Xóa chat và hàng đợi đọc của link cũ.
    pub fn reset(&self, target: &str, status: &str) {
        let mut state = self.state.lock().unwrap();
        state.target = target.to_string();
        state.status = status.to_string();
        state.recent.clear();
        state.speech.clear();
        send_to_all(&mut state, &clear_event(target));
        send_to_all(&mut state, &status_event(status));
    }

    pub fn target(&self) -> String {
        self.state.lock().unwrap().target.clone()
    }

    pub fn add_speech(&self, text: String, sent_at: f64) {
        let mut state = self.state.lock().unwrap();
        if state.speech.len() == MAX_SPEECH_QUEUE {
            state.speech.pop_front();
        }
        state.speech.push_back((sent_at, text));
        self.speech_ready.notify_one();
    }

    /// Lấy câu tiếp theo còn trong hạn đọc; chờ tối đa `timeout`.
    pub fn next_speech(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        loop {
            while let Some((sent_at, text)) = state.speech.pop_front() {
                if now() - sent_at <= MAX_SPEECH_AGE {
                    return Some(text);
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            state = self.speech_ready.wait_timeout(state, remaining).unwrap().0;
        }
    }
}

fn is_url_start(text: &str) -> bool {
    text.starts_with("http://") || text.starts_with("https://") || text.starts_with("www.")
}

fn strip_urls(word: &str) -> &str {
    word.char_indices()
        .find(|(i, _)| is_url_start(&word[*i..]))
        .map_or(word, |(i, _)| &word[..i])
}

fn collapse_repeats(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = None;
    let mut run = 0;
    for ch in text.chars() {
        run = if last == Some(ch) { run + 1 } else { 1 };
        last = Some(ch);
        if run <= 3 {
            out.push(ch);
        }
    }
    out
}

/// Trả về câu cần đọc, hoặc None nếu bình luận không có gì để đọc.
pub fn clean_for_speech(author: &str, speech: &str, max_chars: usize, no_author: bool) -> Option<String> {
    let words: Vec<&str> = speech
        .split_whitespace()
        .map(strip_urls)
        .filter(|word| !word.is_empty())
        .collect();
    let mut text = collapse_repeats(&words.join(" "));
    if !text.chars().any(char::is_alphanumeric) {
        return None;
    }
    if let Some((cut, _)) = text.char_indices().nth(max_chars) {
        text.truncate(cut);
        if let Some(space) = text.rfind(' ') {
            text.truncate(space);
        }
    }
    if no_author {
        return Some(text);
    }
    Some(format!("{} nói: {text}", author.trim_start_matches('@')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(text: &str) -> Option<String> {
        clean_for_speech("@minh", text, 20, false)
    }

    #[test]
    fn speech_text_is_cleaned() {
        assert_eq!(clean("chào bạn").as_deref(), Some("minh nói: chào bạn"));
        assert_eq!(clean("xem https://a.b/c nhé").as_deref(), Some("minh nói: xem nhé"));
        assert_eq!(clean("xem:https://a.b/c").as_deref(), Some("minh nói: xem:"));
        assert_eq!(clean("hahaaaaaaa").as_deref(), Some("minh nói: hahaaa"));
        assert_eq!(clean("😂 !!! ..."), None);
        assert_eq!(clean("https://a.b/c"), None);
        assert_eq!(
            clean("một hai ba bốn năm sáu bảy tám").as_deref(),
            Some("minh nói: một hai ba bốn năm")
        );
        assert_eq!(clean_for_speech("@a", "chào", 20, true).as_deref(), Some("chào"));
    }

    #[test]
    fn speech_queue_drops_old_comments() {
        let hub = Hub::new();
        hub.add_speech("quá 3 phút".into(), now() - 180.0);
        hub.add_speech("còn 1 phút".into(), now() - 60.0);
        assert_eq!(hub.next_speech(Duration::ZERO).as_deref(), Some("còn 1 phút"));
        assert_eq!(hub.next_speech(Duration::ZERO), None);
    }

    #[test]
    fn speech_queue_keeps_newest_when_full() {
        let hub = Hub::new();
        for i in 0..(MAX_SPEECH_QUEUE + 5) {
            hub.add_speech(i.to_string(), now());
        }
        assert_eq!(hub.next_speech(Duration::ZERO).as_deref(), Some("5"));
    }
}

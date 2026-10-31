//! Giữ luồng đọc chat của link hiện tại; đổi link thì dừng luồng cũ.
use crate::chat::{self, ChatError, StopSignal};
use crate::hub::{self, Hub, MAX_SPEECH_AGE, NO_TARGET_STATUS};
use crate::Options;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const RETRY_NOT_LIVE: Duration = Duration::from_secs(30);
const RETRY_NETWORK: Duration = Duration::from_secs(5);
// Đủ để nhận ra chat cũ tải lại khi kết nối lại (mỗi lần khoảng 100 tin)
const MAX_SEEN: usize = 1000;
const SETTINGS_FILE: &str = "settings.json";

pub type Logger = Arc<dyn Fn(String) + Send + Sync>;

pub struct Runner {
    hub: Arc<Hub>,
    options: Options,
    log: Logger,
    stop: Mutex<Option<Arc<StopSignal>>>,
}

impl Runner {
    pub fn new(hub: Arc<Hub>, options: Options, log: Logger) -> Self {
        Runner { hub, options, log, stop: Mutex::new(None) }
    }

    pub fn set_target(&self, target: &str) -> Result<(), String> {
        let target = target.trim().to_string();
        chat::parse_target(&target)?;

        let signal = Arc::new(StopSignal::default());
        {
            let mut current = self.stop.lock().unwrap();
            if let Some(old) = current.replace(signal.clone()) {
                old.stop();
            }
            self.hub.reset(&target, "");
        }
        let (hub, options, log) = (self.hub.clone(), self.options.clone(), self.log.clone());
        let thread_target = target.clone();
        std::thread::spawn(move || chat_loop(&hub, &options, &log, &thread_target, &signal));
        save_target(&target, &self.log);
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(old) = self.stop.lock().unwrap().take() {
            old.stop();
        }
        self.hub.reset("", NO_TARGET_STATUS);
    }
}

fn chat_loop(hub: &Hub, options: &Options, log: &Logger, target: &str, stop: &StopSignal) {
    let mut seen = HashSet::new();
    let mut seen_order = VecDeque::new();
    loop {
        let result = chat::resolve_video_id(target).and_then(|video_id| {
            if stop.is_stopped() {
                return Ok(());
            }
            hub.publish_status("");
            log(format!("Đang đọc chat của video {video_id}"));
            chat::stream(&video_id, stop, |message| {
                if stop.is_stopped() || !seen.insert(message.id.clone()) {
                    return;
                }
                seen_order.push_back(message.id.clone());
                if seen_order.len() > MAX_SEEN {
                    if let Some(oldest) = seen_order.pop_front() {
                        seen.remove(&oldest);
                    }
                }

                hub.publish_chat(&json!({
                    "type": "chat",
                    "author": message.author,
                    "text": message.text,
                    "amount": message.amount,
                }));
                // Chat cũ hơn tải kèm lúc kết nối chỉ hiển thị, không đọc
                if hub::now() - message.timestamp <= MAX_SPEECH_AGE {
                    if let Some(speech) = hub::clean_for_speech(
                        &message.author,
                        &message.speech,
                        options.max_chars,
                        options.no_author,
                    ) {
                        hub.add_speech(speech, message.timestamp);
                    }
                }
            })
        });
        if stop.is_stopped() {
            return;
        }
        let delay = match result {
            Ok(()) => RETRY_NOT_LIVE,
            Err(ChatError::NotLive(message)) => {
                hub.publish_status(&message);
                log(message);
                RETRY_NOT_LIVE
            }
            Err(ChatError::Network(message)) => {
                hub.publish_status("Mất kết nối, đang thử lại...");
                log(format!("Lỗi khi đọc chat: {message}"));
                RETRY_NETWORK
            }
        };
        if stop.wait(delay) {
            return;
        }
    }
}

pub fn load_target() -> Option<String> {
    let text = std::fs::read_to_string(crate::app_dir().join(SETTINGS_FILE)).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    data.get("target")?.as_str().map(String::from)
}

fn save_target(target: &str, log: &Logger) {
    let text = json!({ "target": target }).to_string();
    if let Err(error) = std::fs::write(crate::app_dir().join(SETTINGS_FILE), text) {
        log(format!("Không lưu được link: {error}"));
    }
}

//! Lấy câu từ hàng đợi, tạo giọng đọc và phát ra loa.
use crate::hub::Hub;
use crate::runner::Logger;
use crate::tts::Tts;
use rodio::{Decoder, OutputStream, Sink};
use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(250);
const NEXT_SPEECH_WAIT: Duration = Duration::from_secs(1);
pub const READING_PREFIX: &str = "Đang đọc: ";
// Trả bộ nhớ quá thường xuyên sẽ tốn CPU nạp lại, nên giới hạn số lần
const MIN_TRIM_INTERVAL: Duration = Duration::from_secs(60);

/// Bật/tắt phát tiếng và âm lượng, do giao diện điều khiển.
pub struct PlayerControl {
    enabled: Mutex<bool>,
    changed: Condvar,
    volume_percent: AtomicU32,
}

impl PlayerControl {
    pub fn new(volume_percent: u32) -> Self {
        PlayerControl {
            enabled: Mutex::new(false),
            changed: Condvar::new(),
            volume_percent: AtomicU32::new(volume_percent),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        *self.enabled.lock().unwrap() = enabled;
        self.changed.notify_all();
    }

    pub fn set_volume(&self, percent: u32) {
        self.volume_percent.store(percent, Ordering::Relaxed);
    }

    fn is_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    fn wait_until_enabled(&self) {
        let enabled = self.enabled.lock().unwrap();
        let _unused = self.changed.wait_while(enabled, |enabled| !*enabled).unwrap();
    }

    fn volume(&self) -> f32 {
        self.volume_percent.load(Ordering::Relaxed) as f32 / 100.0
    }
}

/// Chỉ mở thiết bị âm thanh trong lúc đọc để không tốn CPU khi đang chờ bình luận.
fn play(audio: Vec<u8>, control: &PlayerControl) -> Result<(), String> {
    let (_stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
    let player = Sink::try_new(&handle).map_err(|e| e.to_string())?;
    player.set_volume(control.volume());
    player.append(Decoder::new(Cursor::new(audio)).map_err(|e| e.to_string())?);
    while !player.empty() {
        if !control.is_enabled() {
            player.stop();
            return Ok(());
        }
        player.set_volume(control.volume());
        sleep(POLL);
    }
    // Chờ phần âm thanh cuối còn trong bộ đệm của thiết bị
    sleep(POLL);
    Ok(())
}

pub fn spawn(hub: Arc<Hub>, tts: Arc<Mutex<Tts>>, control: Arc<PlayerControl>, log: Logger) {
    std::thread::spawn(move || {
        let mut needs_trim = false;
        let mut last_trim = Instant::now();
        loop {
            if !control.is_enabled() && needs_trim {
                tts.lock().unwrap().release_worker();
                crate::trim_memory();
                needs_trim = false;
                last_trim = Instant::now();
            }
            control.wait_until_enabled();
            let Some(text) = hub.next_speech(NEXT_SPEECH_WAIT) else {
                tts.lock().unwrap().drop_unused_worker();
                if needs_trim && last_trim.elapsed() >= MIN_TRIM_INTERVAL {
                    crate::trim_memory();
                    needs_trim = false;
                    last_trim = Instant::now();
                }
                continue;
            };
            needs_trim = true;
            let audio = tts.lock().unwrap().synthesize(&text);
            match audio {
                Ok(audio) if control.is_enabled() => {
                    log(format!("{READING_PREFIX}{text}"));
                    if let Err(error) = play(audio, &control) {
                        log(format!("Không phát được tiếng đọc: {error}"));
                    }
                }
                Ok(_) => {}
                Err(error) => log(format!("Bỏ qua một bình luận vì không tạo được giọng đọc: {error}")),
            }
        }
    });
}

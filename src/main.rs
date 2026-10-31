//! Đọc to và hiển thị live chat YouTube, chạy cùng OBS.
//!
//! Chạy: ReadComments.exe [link] [--port N] [--voice TÊN] [--vieneu-voice TÊN] [--max-chars N] [--no-author]
#![windows_subsystem = "windows"]

mod chat;
mod edge_token;
mod gui;
mod hub;
mod player;
mod runner;
mod server;
mod tts;
mod vieneu;

use std::path::PathBuf;

const DEFAULT_PORT: u16 = 8765;
const DEFAULT_VOICE: &str = "vi-VN-HoaiMyNeural";
const DEFAULT_VIENEU_VOICE: &str = "Mỹ Duyên";
const DEFAULT_MAX_CHARS: usize = 200;

#[derive(Clone)]
pub struct Options {
    pub target: Option<String>,
    pub port: u16,
    pub voice: String,
    pub vieneu_voice: String,
    pub max_chars: usize,
    pub no_author: bool,
}

fn parse_options() -> Options {
    let mut options = Options {
        target: None,
        port: DEFAULT_PORT,
        voice: DEFAULT_VOICE.to_string(),
        vieneu_voice: DEFAULT_VIENEU_VOICE.to_string(),
        max_chars: DEFAULT_MAX_CHARS,
        no_author: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                if let Some(port) = args.next().and_then(|v| v.parse().ok()) {
                    options.port = port;
                }
            }
            "--voice" => {
                if let Some(voice) = args.next() {
                    options.voice = voice;
                }
            }
            "--vieneu-voice" => {
                if let Some(voice) = args.next() {
                    options.vieneu_voice = voice;
                }
            }
            "--max-chars" => {
                if let Some(max_chars) = args.next().and_then(|v| v.parse().ok()) {
                    options.max_chars = max_chars;
                }
            }
            "--no-author" => options.no_author = true,
            _ => options.target = Some(arg),
        }
    }
    options
}

/// Thư mục chứa file .exe, nơi lưu settings.json và panel.json.
pub fn app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Trả phần bộ nhớ đang không dùng lại cho hệ thống; dùng khi vừa rảnh việc.
pub fn trim_memory() {
    use winapi::um::{processthreadsapi::GetCurrentProcess, winbase::SetProcessWorkingSetSize};
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

/// Để OBS luôn được ưu tiên CPU khi máy bận.
fn yield_cpu_to_other_apps() {
    use winapi::um::{
        processthreadsapi::{GetCurrentProcess, SetPriorityClass},
        winbase::BELOW_NORMAL_PRIORITY_CLASS,
    };
    unsafe {
        SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}

fn main() {
    yield_cpu_to_other_apps();
    gui::run(parse_options());
}

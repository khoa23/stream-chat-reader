//! Chạy VieNeu-TTS trong một tiến trình Python riêng, chỉ khi người dùng chọn nguồn này.
use crate::runner::Logger;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const WORKER_SOURCE: &str = include_str!("vieneu_worker.py");
const PACKAGE: &str = "vieneu==3.8.3";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const INSTALLED_MARKER: &str = ".installed";
const LOG_FILE: &str = "worker.log";
const INSTALL_LOG_FILE: &str = "install.log";

#[derive(Clone, Copy, PartialEq)]
pub enum Model {
    Nano,
    Turbo,
}

impl Model {
    fn key(self) -> &'static str {
        match self {
            Model::Nano => "nano",
            Model::Turbo => "turbo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Model::Nano => "VieNeu Nano",
            Model::Turbo => "VieNeu Turbo",
        }
    }
}

fn base_dir() -> PathBuf {
    crate::app_dir().join("vieneu")
}

fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Chạy lệnh cài đặt, ghi toàn bộ đầu ra vào file nhật ký.
fn run_install_step(mut command: Command, log_path: &Path) -> Result<(), String> {
    let log = File::options()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let status = command
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("mã lỗi {status}"))
    }
}

fn env_python(base: &Path) -> PathBuf {
    base.join("env").join("Scripts").join("python.exe")
}

fn create_env(base: &Path, log_path: &Path) -> Result<(), String> {
    let env_dir = base.join("env");
    let mut last_error = String::new();
    for (program, args) in [("python", &[][..]), ("py", &["-3"][..])] {
        let mut command = hidden_command(program);
        command.args(args).args(["-m", "venv"]).arg(&env_dir);
        match run_install_step(command, log_path) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(format!("không tạo được môi trường Python, cần Python 3.10 trở lên ({last_error})"))
}

/// Cài VieNeu ở lần dùng đầu tiên.
fn ensure_installed(base: &Path, log: &Logger) -> Result<(), String> {
    if base.join(INSTALLED_MARKER).exists() {
        return Ok(());
    }
    fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let log_path = base.join(INSTALL_LOG_FILE);
    log("Đang cài VieNeu lần đầu (cần Internet, khoảng 800 MB, mất vài phút)...".to_string());
    if !env_python(base).exists() {
        create_env(base, &log_path)?;
    }
    let mut pip = hidden_command(env_python(base));
    pip.args(["-m", "pip", "install", "--disable-pip-version-check", PACKAGE]);
    run_install_step(pip, &log_path)
        .map_err(|e| format!("cài VieNeu thất bại ({e}), xem {}", log_path.display()))?;
    fs::write(base.join(INSTALLED_MARKER), PACKAGE).map_err(|e| e.to_string())?;
    log("Đã cài xong VieNeu.".to_string());
    Ok(())
}

pub struct Worker {
    model: Model,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Worker {
    pub fn start(model: Model, voice: &str, log: &Logger) -> Result<Worker, String> {
        let base = base_dir();
        ensure_installed(&base, log)?;

        let script = base.join("worker.py");
        fs::write(&script, WORKER_SOURCE).map_err(|e| e.to_string())?;
        let log_path = base.join(LOG_FILE);
        let stderr = File::create(&log_path).map_err(|e| e.to_string())?;
        // Đã tải xong mô hình thì không kết nối mạng nữa
        let downloaded = base.join(format!(".{}-ready", model.key()));
        let offline = downloaded.exists();

        log(if offline {
            format!("Đang nạp {}...", model.label())
        } else {
            format!("Đang tải {} lần đầu (khoảng 300 MB)...", model.label())
        });
        let mut command = hidden_command(env_python(&base));
        command
            .arg(&script)
            .args([model.key(), voice])
            .env("HF_HOME", base.join("hf"))
            .env("HF_HUB_DISABLE_TELEMETRY", "1")
            .env("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
            .env("GRADIO_ANALYTICS_ENABLED", "False")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        if offline {
            command.env("HF_HUB_OFFLINE", "1");
        }
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("không mở được kênh ghi")?;
        let stdout = child.stdout.take().ok_or("không mở được kênh đọc")?;
        let mut worker = Worker { model, child, stdin, stdout: BufReader::new(stdout) };

        let ready = worker.read_header().map_err(|error| {
            // Có thể mô hình trong máy đã hỏng: lần sau cho phép tải lại
            let _ = fs::remove_file(&downloaded);
            format!("{error}, xem {}", log_path.display())
        })?;
        if !offline {
            let _ = fs::write(&downloaded, "");
        }
        let used_voice = ready.get("voice").and_then(Value::as_str).unwrap_or("");
        log(format!("{} đã sẵn sàng, giọng {used_voice}.", model.label()));
        Ok(worker)
    }

    pub fn model(&self) -> Model {
        self.model
    }

    fn read_header(&mut self) -> Result<Value, String> {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("tiến trình VieNeu đã dừng".to_string());
        }
        serde_json::from_str(&line).map_err(|e| e.to_string())
    }

    /// Trả về âm thanh WAV. Lỗi nằm trong Err kèm cờ cho biết tiến trình còn dùng được không.
    pub fn synthesize(&mut self, text: &str) -> Result<Vec<u8>, (String, bool)> {
        let dead = |error: String| (error, false);
        let request = json!({ "text": text }).to_string() + "\n";
        self.stdin.write_all(request.as_bytes()).map_err(|e| dead(e.to_string()))?;
        self.stdin.flush().map_err(|e| dead(e.to_string()))?;

        let header = self.read_header().map_err(dead)?;
        if let Some(error) = header.get("error").and_then(Value::as_str) {
            return Err((error.to_string(), true));
        }
        let size = header
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| dead("VieNeu trả lời không hợp lệ".to_string()))?;
        let mut audio = vec![0; size as usize];
        self.stdout.read_exact(&mut audio).map_err(|e| dead(e.to_string()))?;
        Ok(audio)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

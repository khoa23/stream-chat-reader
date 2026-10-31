//! Bảng điều khiển: nhập link livestream, bắt đầu/dừng, bật tiếng và chỉnh âm lượng.
use crate::hub::Hub;
use crate::player::{self, PlayerControl, READING_PREFIX};
use crate::runner::{self, Logger, Runner};
use crate::server::{self, Shared};
use crate::tts::{Engine, EngineChoice, Tts};
use crate::Options;
use native_windows_gui as nwg;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

const PANEL_FILE: &str = "panel.json";
const MARGIN: i32 = 12;
const ROW_HEIGHT: i32 = 28;
const LABEL_WIDTH: i32 = 110;
const VALUE_WIDTH: i32 = 44;
const BUTTON_WIDTH: i32 = 90;
const MIN_WIDTH: i32 = 420;
const MIN_HEIGHT: i32 = 300;
// Ô chữ của Windows mặc định giới hạn khoảng 32 nghìn ký tự, đầy rồi sẽ không nhận thêm
const MAX_LOG_CHARS: u32 = 20_000;
// ID nhóm icon do build.rs nhúng vào file .exe
const ICON_RESOURCE_ID: u16 = 1;
const SMALL_ICON_SIZE: i32 = 16;
const BIG_ICON_SIZE: i32 = 32;

/// Việc từ các luồng nền, được xử lý trên luồng giao diện.
enum UiEvent {
    Log(String),
    TargetChanged(String),
}

#[derive(Default)]
struct Controls {
    window: nwg::Window,
    url_label: nwg::Label,
    url: nwg::TextInput,
    engine_label: nwg::Label,
    engine: nwg::ComboBox<&'static str>,
    start: nwg::Button,
    stop: nwg::Button,
    play_here: nwg::CheckBox,
    volume_label: nwg::Label,
    volume: nwg::TrackBar,
    volume_value: nwg::Label,
    log: nwg::TextBox,
    notice: nwg::Notice,
}

fn load_volume() -> u32 {
    std::fs::read_to_string(crate::app_dir().join(PANEL_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|data| data.get("volume")?.as_u64())
        .map_or(100, |volume| volume.min(100) as u32)
}

fn load_engine_index() -> usize {
    std::fs::read_to_string(crate::app_dir().join(PANEL_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|data| {
            let key = data.get("engine")?.as_str()?.to_string();
            Engine::ALL.iter().position(|engine| engine.key() == key)
        })
        .unwrap_or(0)
}

fn save_panel(volume: u32, engine: Engine) {
    // Không lưu được thì lần sau dùng mức mặc định
    let panel = json!({ "volume": volume, "engine": engine.key() });
    let _ = std::fs::write(crate::app_dir().join(PANEL_FILE), panel.to_string());
}

/// Đặt icon trên thanh tiêu đề và thanh tác vụ từ tài nguyên trong file .exe.
fn set_window_icon(window: &nwg::Window) {
    use std::ptr::null;
    use winapi::um::{
        libloaderapi::GetModuleHandleW,
        winuser::{LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, MAKEINTRESOURCEW, WM_SETICON},
    };
    let Some(hwnd) = window.handle.hwnd() else {
        return;
    };
    for (kind, size) in [(ICON_SMALL, SMALL_ICON_SIZE), (ICON_BIG, BIG_ICON_SIZE)] {
        unsafe {
            let icon = LoadImageW(
                GetModuleHandleW(null()),
                MAKEINTRESOURCEW(ICON_RESOURCE_ID),
                IMAGE_ICON,
                size,
                size,
                0,
            );
            SendMessageW(hwnd, WM_SETICON, kind as usize, icon as isize);
        }
    }
}

/// Nếu nhật ký quá dài, trả về phần nửa sau (bắt đầu từ đầu một dòng) để giữ lại.
fn trim_log(text: &str) -> &str {
    let mut middle = text.len() / 2;
    while !text.is_char_boundary(middle) {
        middle += 1;
    }
    text[middle..]
        .find("\r\n")
        .map_or(&text[middle..], |line_end| &text[middle + line_end + 2..])
}

fn build_controls(controls: &mut Controls, volume: u32, engine_index: usize) -> Result<(), nwg::NwgError> {
    let mut font = nwg::Font::default();
    nwg::Font::builder().family("Segoe UI").size(18).build(&mut font)?;
    nwg::Font::set_global_default(Some(font));

    nwg::Window::builder()
        .flags(nwg::WindowFlags::MAIN_WINDOW)
        .size((576, 480))
        .center(true)
        .title("Đọc comment YouTube")
        .build(&mut controls.window)?;
    set_window_icon(&controls.window);
    let parent = &controls.window;

    nwg::Label::builder().text("Link livestream:").parent(parent).build(&mut controls.url_label)?;
    nwg::TextInput::builder()
        .placeholder_text(Some("https://www.youtube.com/watch?v=..."))
        .parent(parent)
        .build(&mut controls.url)?;
    nwg::Label::builder().text("Nguồn đọc:").parent(parent).build(&mut controls.engine_label)?;
    nwg::ComboBox::builder()
        .collection(Engine::ALL.iter().map(|engine| engine.label()).collect())
        .selected_index(Some(engine_index))
        .parent(parent)
        .build(&mut controls.engine)?;
    nwg::Button::builder().text("Bắt đầu").parent(parent).build(&mut controls.start)?;
    nwg::Button::builder()
        .text("Dừng")
        .parent(parent)
        .build(&mut controls.stop)?;
    controls.stop.set_enabled(false);
    nwg::CheckBox::builder()
        .text("Phát tiếng đọc ra loa máy này")
        .check_state(nwg::CheckBoxState::Checked)
        .parent(parent)
        .build(&mut controls.play_here)?;
    nwg::Label::builder().text("Âm lượng:").parent(parent).build(&mut controls.volume_label)?;
    nwg::TrackBar::builder()
        .range(Some(0..100))
        .pos(Some(volume as usize))
        .flags(nwg::TrackBarFlags::VISIBLE | nwg::TrackBarFlags::HORIZONTAL)
        .parent(parent)
        .build(&mut controls.volume)?;
    nwg::Label::builder()
        .text(&format!("{volume}%"))
        .parent(parent)
        .build(&mut controls.volume_value)?;
    nwg::TextBox::builder()
        .flags(
            nwg::TextBoxFlags::VISIBLE
                | nwg::TextBoxFlags::VSCROLL
                | nwg::TextBoxFlags::AUTOVSCROLL,
        )
        .readonly(true)
        .parent(parent)
        .build(&mut controls.log)?;
    nwg::Notice::builder().parent(parent).build(&mut controls.notice)?;
    Ok(())
}

fn layout(controls: &Controls) {
    let (width, height) = controls.window.size();
    let width = (width as i32).max(MIN_WIDTH);
    let height = (height as i32).max(MIN_HEIGHT);
    let inner = width - 2 * MARGIN;

    let mut y = MARGIN;
    controls.url_label.set_position(MARGIN, y + 4);
    controls.url_label.set_size(LABEL_WIDTH as u32, ROW_HEIGHT as u32 - 4);
    controls.url.set_position(MARGIN + LABEL_WIDTH, y);
    controls.url.set_size((inner - LABEL_WIDTH) as u32, ROW_HEIGHT as u32);

    y += ROW_HEIGHT + 8;
    controls.engine_label.set_position(MARGIN, y + 4);
    controls.engine_label.set_size(LABEL_WIDTH as u32, ROW_HEIGHT as u32 - 4);
    controls.engine.set_position(MARGIN + LABEL_WIDTH, y);
    controls.engine.set_size((inner - LABEL_WIDTH) as u32, ROW_HEIGHT as u32);

    y += ROW_HEIGHT + 8;
    controls.start.set_position(MARGIN, y);
    controls.start.set_size(BUTTON_WIDTH as u32, ROW_HEIGHT as u32);
    controls.stop.set_position(MARGIN + BUTTON_WIDTH + 8, y);
    controls.stop.set_size(BUTTON_WIDTH as u32, ROW_HEIGHT as u32);
    let check_x = MARGIN + 2 * BUTTON_WIDTH + 24;
    controls.play_here.set_position(check_x, y);
    controls.play_here.set_size((width - MARGIN - check_x) as u32, ROW_HEIGHT as u32);

    y += ROW_HEIGHT + 8;
    controls.volume_label.set_position(MARGIN, y + 4);
    controls.volume_label.set_size(LABEL_WIDTH as u32, ROW_HEIGHT as u32 - 4);
    controls.volume.set_position(MARGIN + LABEL_WIDTH, y);
    controls.volume.set_size((inner - LABEL_WIDTH - VALUE_WIDTH) as u32, ROW_HEIGHT as u32);
    controls.volume_value.set_position(width - MARGIN - VALUE_WIDTH + 4, y + 4);
    controls.volume_value.set_size(VALUE_WIDTH as u32 - 4, ROW_HEIGHT as u32 - 4);

    y += ROW_HEIGHT + 8;
    controls.log.set_position(MARGIN, y);
    controls.log.set_size(inner as u32, (height - y - MARGIN) as u32);
}

pub fn run(options: Options) {
    nwg::init().expect("Không khởi tạo được giao diện");
    let saved_volume = load_volume();
    let saved_engine = load_engine_index();
    let mut controls = Controls::default();
    build_controls(&mut controls, saved_volume, saved_engine).expect("Không tạo được giao diện");
    let controls = Rc::new(controls);

    // Hàng đợi từ luồng nền về giao diện
    let events: Arc<Mutex<Vec<UiEvent>>> = Arc::default();
    let sender = controls.notice.sender();
    let log: Logger = {
        let events = events.clone();
        Arc::new(move |line| {
            events.lock().unwrap().push(UiEvent::Log(line));
            sender.notice();
        })
    };

    let hub = Arc::new(Hub::new());
    let runner = Arc::new(Runner::new(hub.clone(), options.clone(), log.clone()));
    let engine_choice: EngineChoice = Arc::new(saved_engine.into());
    let tts = Arc::new(Mutex::new(Tts::new(
        options.voice.clone(),
        options.vieneu_voice.clone(),
        engine_choice.clone(),
        log.clone(),
    )));
    let control = Arc::new(PlayerControl::new(saved_volume));
    player::spawn(hub.clone(), tts.clone(), control.clone(), log.clone());

    let target_events = events.clone();
    let started = server::start(Shared {
        hub,
        runner: runner.clone(),
        tts,
        port: options.port,
        log: log.clone(),
        on_target_changed: Box::new(move |target| {
            target_events.lock().unwrap().push(UiEvent::TargetChanged(target));
            sender.notice();
        }),
    });
    if let Err(error) = started {
        log(format!("Không mở được trang chat ở cổng {}: {error}", options.port));
    }

    let running = Rc::new(Cell::new(false));
    let last_log = Rc::new(RefCell::new(String::new()));

    let append_log = {
        let controls = controls.clone();
        let last_log = last_log.clone();
        move |line: &str| {
            // Lỗi lặp lại mỗi lần thử lại chỉ cần hiện một lần; câu đọc thì luôn hiện
            if *last_log.borrow() == line && !line.starts_with(READING_PREFIX) {
                return;
            }
            *last_log.borrow_mut() = line.to_string();
            if controls.log.len() > MAX_LOG_CHARS {
                let text = controls.log.text();
                controls.log.set_text(trim_log(&text));
            }
            controls.log.appendln(line);
            controls.log.scroll_lastline();
        }
    };

    let update_state = {
        let controls = controls.clone();
        let running = running.clone();
        let control = control.clone();
        move || {
            controls.start.set_text(if running.get() { "Áp dụng" } else { "Bắt đầu" });
            controls.stop.set_enabled(running.get());
            let play_here = controls.play_here.check_state() == nwg::CheckBoxState::Checked;
            control.set_enabled(running.get() && play_here);
        }
    };

    let apply = {
        let controls = controls.clone();
        let runner = runner.clone();
        let running = running.clone();
        let append_log = append_log.clone();
        let update_state = update_state.clone();
        move || {
            let target = controls.url.text();
            if target.trim().is_empty() {
                append_log("Hãy nhập link livestream.");
                controls.url.set_focus();
                return;
            }
            match runner.set_target(&target) {
                Ok(()) => running.set(true),
                Err(error) => append_log(&error),
            }
            update_state();
        }
    };

    let handler_controls = controls.clone();
    let handler_apply = apply.clone();
    let window_handle = controls.window.handle;
    let handler = nwg::full_bind_event_handler(&window_handle, move |event, _data, handle| {
        use nwg::Event as E;
        let c = &handler_controls;
        match event {
            E::OnWindowClose if handle == c.window => {
                control.set_enabled(false);
                let engine = Engine::ALL[engine_choice.load(Ordering::Relaxed)];
                save_panel(c.volume.pos() as u32, engine);
                nwg::stop_thread_dispatch();
            }
            E::OnResize | E::OnWindowMaximize if handle == c.window => layout(c),
            E::OnButtonClick if handle == c.start => handler_apply(),
            E::OnButtonClick if handle == c.stop => {
                runner.stop();
                running.set(false);
                append_log("Đã dừng.");
                update_state();
                crate::trim_memory();
            }
            E::OnButtonClick if handle == c.play_here => update_state(),
            E::OnComboxBoxSelection if handle == c.engine => {
                if let Some(index) = c.engine.selection() {
                    engine_choice.store(index, Ordering::Relaxed);
                    append_log(&format!("Nguồn đọc: {}", Engine::ALL[index].label()));
                }
            }
            E::OnHorizontalScroll => {
                let volume = c.volume.pos() as u32;
                c.volume_value.set_text(&format!("{volume}%"));
                control.set_volume(volume);
            }
            E::OnNotice if handle == c.notice => {
                let pending: Vec<UiEvent> = std::mem::take(&mut *events.lock().unwrap());
                for event in pending {
                    match event {
                        UiEvent::Log(line) => append_log(&line),
                        UiEvent::TargetChanged(target) => {
                            c.url.set_text(&target);
                            running.set(true);
                            update_state();
                        }
                    }
                }
            }
            _ => {}
        }
    });

    layout(&controls);
    if let Some(target) = options.target.or_else(runner::load_target) {
        controls.url.set_text(&target);
        if std::env::args().skip(1).any(|arg| arg == target) {
            apply();
        }
    }
    controls.window.set_visible(true);
    crate::trim_memory();
    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
}

#[cfg(test)]
mod tests {
    use super::trim_log;

    #[test]
    fn trimmed_log_starts_at_a_line_boundary() {
        assert_eq!(trim_log("một một\r\nhai hai\r\nba\r\n"), "ba\r\n");
        assert_eq!(trim_log("abcdefgh"), "efgh");
        // Điểm giữa rơi vào giữa một ký tự nhiều byte thì lùi tới ký tự kế tiếp
        assert_eq!(trim_log("aaộbb"), "bb");
    }
}

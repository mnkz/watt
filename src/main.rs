#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod model;
mod price;
mod scan;
mod ui;

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use eframe::egui::{self, FontData, FontDefinitions, FontFamily, IconData, ViewportBuilder};
use serde::{Deserialize, Serialize};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::model::{Period, Provider, Snapshot, month_start};
use crate::scan::{Shared, data_dir};
use crate::ui::{BG, STROKE, TEXT, period_view, tooltip_line};

const WIN_W: f32 = 400.0;
const WIN_H: f32 = 680.0;

static MUTEX: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

fn main() {
    if !claim_instance() {
        message_box("Watt", "Watt is already in the tray.");
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        log_line(&format!("panic: {info}"));
    }));
    let options = native_options();
    let err = eframe::run_native(
        "watt",
        options,
        Box::new(|cc| Ok(Box::new(WattApp::new(cc)))),
    );
    match err {
        Ok(()) => log_line("window closed"),
        Err(err) => {
            let msg = format!("Watt could not start.\n{err}");
            log_line(&msg);
            message_box("Watt", &msg);
        }
    }
}

struct Wake {
    pending: Mutex<bool>,
    cvar: Condvar,
}

impl Wake {
    fn poke(&self) {
        let mut guard = self.pending.lock().unwrap_or_else(|err| err.into_inner());
        *guard = true;
        self.cvar.notify_one();
    }
}

enum Cmd {
    Toggle(Option<tray_icon::Rect>),
    Open,
    Quit,
}

pub struct WattApp {
    shared: Arc<Shared>,
    wake: Arc<Wake>,
    tray: Option<TrayIcon>,
    tooltip: String,
    snap: Arc<Snapshot>,
    seen_gen: u64,
    period: Period,
    custom_start: NaiveDate,
    custom_end: NaiveDate,
    start_text: String,
    end_text: String,
    abos: [f64; 5],
    expanded: Option<Provider>,
    abos_open: bool,
    visible: bool,
    seen_focus: bool,
    hidden_by_blur_at: Option<Instant>,
    suppress_blur_until: Instant,
    icon_rect: Option<tray_icon::Rect>,
    placed: bool,
    start_in_tray: bool,
    tray_hold: u8,
    quitting: bool,
    rx: Receiver<Cmd>,
    last_read: Option<chrono::DateTime<Local>>,
}

impl WattApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_look(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        let shared = Shared::new();
        let wake = Arc::new(Wake {
            pending: Mutex::new(false),
            cvar: Condvar::new(),
        });
        let cfg = Config::load();
        let today = Local::now().date_naive();
        let custom_start = parse_day(&cfg.custom_start).unwrap_or_else(|| month_start(today));
        let custom_end = parse_day(&cfg.custom_end).unwrap_or(today);
        let tray = install_tray(tx.clone(), cc.egui_ctx.clone());
        let start_in_tray = launched_in_tray();
        let app = Self {
            shared: Arc::clone(&shared),
            wake: Arc::clone(&wake),
            tray,
            tooltip: "Watt".to_string(),
            snap: Arc::new(Snapshot::default()),
            seen_gen: 0,
            period: Period::from_key(&cfg.period).unwrap_or(Period::Today),
            custom_start,
            custom_end,
            start_text: custom_start.format("%Y-%m-%d").to_string(),
            end_text: custom_end.format("%Y-%m-%d").to_string(),
            abos: cfg.abos(),
            expanded: None,
            abos_open: false,
            visible: !start_in_tray,
            seen_focus: false,
            hidden_by_blur_at: None,
            suppress_blur_until: Instant::now() + Duration::from_millis(1200),
            icon_rect: None,
            placed: false,
            start_in_tray,
            tray_hold: 0,
            quitting: false,
            rx,
            last_read: None,
        };
        spawn_scan(shared, wake, cc.egui_ctx.clone());
        app
    }

    fn hide(&mut self, ctx: &egui::Context) {
        self.visible = false;
        self.seen_focus = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn hide_from_blur(&mut self, ctx: &egui::Context) {
        self.hidden_by_blur_at = Some(Instant::now());
        self.hide(ctx);
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.start_in_tray = false;
        self.visible = true;
        self.seen_focus = false;
        self.hidden_by_blur_at = None;
        self.suppress_blur_until = Instant::now() + Duration::from_millis(400);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.place(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn place(&mut self, ctx: &egui::Context) {
        let ppp = ctx.pixels_per_point().max(1.0);
        let area = work_area(ppp);
        let mut size = ctx
            .input(|i| i.viewport().inner_rect.map(|r| r.size()))
            .unwrap_or(egui::vec2(WIN_W, WIN_H));
        if size.x < 200.0 {
            size.x = WIN_W;
        }
        let max_h = (area.height() - 16.0).max(420.0);
        if size.y > max_h || size.y < 200.0 {
            size.y = max_h.clamp(420.0, WIN_H);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
        let (mut x, mut y) = match self.icon_rect.filter(rect_usable) {
            Some(icon) => {
                let right = (icon.position.x as f32 + icon.size.width as f32) / ppp;
                let top = icon.position.y as f32 / ppp;
                let bottom = top + icon.size.height as f32 / ppp;
                let x = right - size.x;
                let above = top - size.y - 8.0;
                let y = if above >= area.top() {
                    above
                } else {
                    bottom + 8.0
                };
                (x, y)
            }
            None => (area.right() - size.x - 12.0, area.bottom() - size.y - 12.0),
        };
        x = x.clamp(
            area.left() + 4.0,
            (area.right() - size.x - 4.0).max(area.left() + 4.0),
        );
        y = y.clamp(
            area.top() + 4.0,
            (area.bottom() - size.y - 4.0).max(area.top() + 4.0),
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
        self.placed = true;
    }

    fn refresh(&self) {
        if !self.shared.scanning.load(Ordering::Relaxed) {
            self.wake.poke();
        }
    }

    fn persist(&self) {
        Config::from_app(self).save();
    }

    fn pull_snapshot(&mut self) {
        let generation = self.shared.generation.load(Ordering::Relaxed);
        if generation == self.seen_gen {
            return;
        }
        self.seen_gen = generation;
        if let Ok(guard) = self.shared.snapshot.lock() {
            self.snap = Arc::clone(&guard);
        }
        if !self.shared.scanning.load(Ordering::Relaxed) {
            self.last_read = Some(Local::now());
        }
    }

    fn sync_tooltip(&mut self) {
        let today = Local::now().date_naive();
        let view = period_view(
            &self.snap,
            self.period,
            today,
            self.custom_start,
            self.custom_end,
            &self.abos,
        );
        let text = tooltip_line(self.period, view.tokens, view.api_usd);
        if text == self.tooltip {
            return;
        }
        self.tooltip = text.clone();
        if let Some(tray) = &self.tray {
            let _ = tray.set_tooltip(Some(text));
        }
    }

    fn handle_commands(&mut self, ctx: &egui::Context) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Cmd::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Cmd::Open => self.show(ctx),
                Cmd::Toggle(rect) => {
                    if let Some(rect) = rect {
                        self.icon_rect = Some(rect);
                    }
                    if self.visible {
                        self.hide(ctx);
                    } else if self
                        .hidden_by_blur_at
                        .is_some_and(|at| at.elapsed() < Duration::from_millis(450))
                    {
                        self.hidden_by_blur_at = None;
                    } else {
                        self.show(ctx);
                    }
                }
            }
        }
    }
}

impl eframe::App for WattApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_commands(ctx);
        if self.start_in_tray && self.tray_hold < 4 {
            self.tray_hold += 1;
            self.hide(ctx);
        }
        if !self.placed {
            self.place(ctx);
        }
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
        if self.visible {
            if focused {
                self.seen_focus = true;
            } else if self.seen_focus && Instant::now() > self.suppress_blur_until {
                self.hide_from_blur(ctx);
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.quitting {
                return;
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide(ctx);
        }
        if self.visible && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.hide(ctx);
        }
        self.pull_snapshot();
        self.sync_tooltip();
        if self.shared.scanning.load(Ordering::Relaxed) && self.visible {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
        self.sync_tooltip();
    }
}

fn spawn_scan(shared: Arc<Shared>, wake: Arc<Wake>, ctx: egui::Context) {
    std::thread::Builder::new()
        .name("watt-scan".into())
        .spawn(move || {
            loop {
                shared.scanning.store(true, Ordering::Relaxed);
                ctx.request_repaint();
                log_line("scan started");
                let job = Arc::clone(&shared);
                let result = std::panic::catch_unwind(AssertUnwindSafe(move || {
                    scan::run_scan(&job);
                }));
                if let Err(payload) = result {
                    log_line(&format!("scan: {}", panic_message(payload.as_ref())));
                } else {
                    let summary = shared.snapshot.lock().map(|snap| {
                        format!(
                            "scan finished · {} files · {} calls",
                            snap.files, snap.calls
                        )
                    });
                    log_line(&summary.unwrap_or_else(|_| "scan finished".to_string()));
                }
                shared.scanning.store(false, Ordering::Relaxed);
                ctx.request_repaint();
                let guard = wake.pending.lock().unwrap_or_else(|err| err.into_inner());
                let waited =
                    wake.cvar
                        .wait_timeout_while(guard, Duration::from_secs(60), |pending| !*pending);
                let mut guard = match waited {
                    Ok((guard, _)) => guard,
                    Err(err) => err.into_inner().0,
                };
                *guard = false;
            }
        })
        .ok();
}

fn install_tray(tx: Sender<Cmd>, ctx: egui::Context) -> Option<TrayIcon> {
    let tx_click = tx.clone();
    let ctx_click = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            rect,
            ..
        } = event
        {
            let _ = tx_click.send(Cmd::Toggle(Some(rect)));
            ctx_click.request_repaint();
        }
    }));
    let tx_menu = tx;
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == "quit" {
            let _ = tx_menu.send(Cmd::Quit);
        } else if event.id == "open" {
            let _ = tx_menu.send(Cmd::Open);
        }
        ctx.request_repaint();
    }));

    let open = Box::leak(Box::new(MenuItem::with_id("open", "Open", true, None)));
    let quit = Box::leak(Box::new(MenuItem::with_id("quit", "Quit", true, None)));
    let menu = Menu::new();
    if menu.append(open).is_err() || menu.append(quit).is_err() {
        log_line("tray menu unavailable");
    }
    let icon = tray_icon::Icon::from_rgba(watt_mark(32), 32, 32).ok()?;
    match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon(icon)
        .with_tooltip("Watt")
        .build()
    {
        Ok(tray) => Some(tray),
        Err(err) => {
            log_line(&format!("icon: {err}"));
            None
        }
    }
}

fn install_look(ctx: &egui::Context) {
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        let mut fonts = FontDefinitions::default();
        fonts
            .font_data
            .insert("Segoe".to_string(), Arc::new(FontData::from_owned(bytes)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, "Segoe".to_string());
        }
        ctx.set_fonts(fonts);
    }
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            style.visuals.dark_mode = true;
            style.visuals.panel_fill = BG;
            style.visuals.window_fill = BG;
            style.visuals.extreme_bg_color = BG;
            style.visuals.faint_bg_color = gray(12);
            style.visuals.code_bg_color = BG;
            style.visuals.override_text_color = Some(TEXT);
            style.visuals.hyperlink_color = TEXT;
            style.visuals.window_stroke = egui::Stroke::new(1.0, STROKE);
            style.visuals.window_corner_radius = egui::CornerRadius::ZERO;
            style.visuals.menu_corner_radius = egui::CornerRadius::ZERO;
            style.visuals.window_shadow = egui::epaint::Shadow::NONE;
            style.visuals.popup_shadow = egui::epaint::Shadow::NONE;
            style.visuals.selection.bg_fill = TEXT;
            style.visuals.selection.stroke.color = BG;
            paint_widget(&mut style.visuals.widgets.noninteractive);
            paint_widget(&mut style.visuals.widgets.inactive);
            paint_widget(&mut style.visuals.widgets.hovered);
            paint_widget(&mut style.visuals.widgets.active);
            paint_widget(&mut style.visuals.widgets.open);
            style.visuals.widgets.noninteractive.bg_fill = BG;
            style.visuals.widgets.inactive.bg_fill = BG;
            style.visuals.widgets.hovered.bg_fill = gray(18);
            style.visuals.widgets.active.bg_fill = TEXT;
            style.visuals.widgets.active.fg_stroke.color = BG;
            style.visuals.widgets.open.bg_fill = gray(18);
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(8.0, 4.0);
            style.spacing.scroll.bar_width = 4.0;
            style.animation_time = 0.08;
        });
    }
}

fn paint_widget(visual: &mut egui::style::WidgetVisuals) {
    visual.corner_radius = egui::CornerRadius::same(2);
    visual.bg_stroke = egui::Stroke::new(1.0, STROKE);
    visual.fg_stroke = egui::Stroke::new(1.0, TEXT);
}

fn gray(v: u8) -> egui::Color32 {
    egui::Color32::from_rgb(v, v, v)
}

fn native_options() -> eframe::NativeOptions {
    let mark = watt_mark(64);
    let icon = IconData {
        rgba: mark,
        width: 64,
        height: 64,
    };
    eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("Watt")
            .with_inner_size([WIN_W, WIN_H])
            .with_min_inner_size([360.0, 480.0])
            .with_decorations(false)
            .with_transparent(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_visible(!launched_in_tray())
            .with_resizable(true)
            .with_active(!launched_in_tray())
            .with_icon(std::sync::Arc::new(icon)),
        centered: false,
        run_and_return: true,
        ..Default::default()
    }
}

fn launched_in_tray() -> bool {
    std::env::args().skip(1).any(|arg| arg == "--tray")
}

fn watt_mark(size: u32) -> Vec<u8> {
    let mut px = vec![0u8; (size * size * 4) as usize];
    let w = size as f32;
    let m = w * 0.18;
    let pts = [
        (m, m + w * 0.06),
        (w * 0.32, w - m),
        (w * 0.50, w * 0.46),
        (w * 0.68, w - m),
        (w - m, m + w * 0.06),
    ];
    let rad = (w * 0.045).max(1.2);
    for seg in pts.windows(2) {
        stroke(&mut px, size, seg[0], seg[1], rad);
    }
    px
}

fn stroke(px: &mut [u8], size: u32, a: (f32, f32), b: (f32, f32), rad: f32) {
    let steps = (size as i32 * 2).max(24);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = a.0 + (b.0 - a.0) * t;
        let y = a.1 + (b.1 - a.1) * t;
        disk(px, size, x, y, rad);
    }
}

fn disk(px: &mut [u8], size: u32, cx: f32, cy: f32, rad: f32) {
    let r = rad.ceil() as i32 + 1;
    let x0 = cx as i32 - r;
    let y0 = cy as i32 - r;
    for y in y0..=y0 + r * 2 {
        for x in x0..=x0 + r * 2 {
            if x < 0 || y < 0 || x >= size as i32 || y >= size as i32 {
                continue;
            }
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= rad * rad {
                let i = ((y as u32 * size + x as u32) * 4) as usize;
                px[i] = 244;
                px[i + 1] = 244;
                px[i + 2] = 242;
                px[i + 3] = 255;
            }
        }
    }
}

fn rect_usable(rect: &tray_icon::Rect) -> bool {
    rect.size.width > 0
        && rect.size.height > 0
        && (rect.position.x != 0.0 || rect.position.y != 0.0)
}

fn work_area(ppp: f32) -> egui::Rect {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut rect = RECT::default();
    unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut rect as *mut RECT as *mut _, 0);
    }
    let s = ppp.max(1.0);
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
    }
    egui::Rect::from_min_max(
        egui::pos2(rect.left as f32 / s, rect.top as f32 / s),
        egui::pos2(rect.right as f32 / s, rect.bottom as f32 / s),
    )
}

fn claim_instance() -> bool {
    use windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name = wide("Local\\Watt");
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        if handle.is_null() {
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return false;
        }
        MUTEX.store(handle, Ordering::Relaxed);
        true
    }
}

fn message_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
    let title = wide(title);
    let text = wide(text);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn log_line(text: &str) {
    use std::io::Write;
    let path = data_dir().join("watt.log");
    let line = format!("{} {text}\n", Local::now().format("%Y-%m-%d %H:%M:%S"));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown".to_string()
    }
}

fn parse_day(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

#[derive(Serialize, Deserialize)]
struct Config {
    period: String,
    custom_start: String,
    custom_end: String,
    abos: std::collections::BTreeMap<String, f64>,
}

impl Config {
    fn load() -> Self {
        let path = data_dir().join("config.json");
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(cfg) = serde_json::from_str::<Config>(&text)
        {
            return cfg;
        }
        let today = Local::now().date_naive();
        let start = month_start(today);
        Self {
            period: "today".to_string(),
            custom_start: start.format("%Y-%m-%d").to_string(),
            custom_end: today.format("%Y-%m-%d").to_string(),
            abos: Provider::all()
                .into_iter()
                .map(|p| (p.key().to_string(), default_abo(p)))
                .collect(),
        }
    }

    fn abos(&self) -> [f64; 5] {
        let mut out = [0.0; 5];
        for (i, provider) in Provider::all().into_iter().enumerate() {
            out[i] = self
                .abos
                .get(provider.key())
                .copied()
                .unwrap_or_else(|| default_abo(provider));
        }
        out
    }

    fn from_app(app: &WattApp) -> Self {
        Self {
            period: app.period.key().to_string(),
            custom_start: app.custom_start.format("%Y-%m-%d").to_string(),
            custom_end: app.custom_end.format("%Y-%m-%d").to_string(),
            abos: Provider::all()
                .into_iter()
                .enumerate()
                .map(|(i, p)| (p.key().to_string(), app.abos[i]))
                .collect(),
        }
    }

    fn save(&self) {
        let path = data_dir().join("config.json");
        let tmp = data_dir().join("config.json.tmp");
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Grok starts on Premium. The others take the first tier. All of it is editable.
fn default_abo(provider: Provider) -> f64 {
    match provider {
        Provider::Grok => 30.0,
        other => other.default_abo(),
    }
}

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;
use self_update::update::ReleaseUpdate;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

const MAX_LOG_LINES: usize = 1000;

// Release asset name per platform
#[cfg(target_os = "macos")]
const PROXHY_BIN_NAME: &str = "proxhy-macos";
#[cfg(target_os = "linux")]
const PROXHY_BIN_NAME: &str = "proxhy-linux";
#[cfg(target_os = "windows")]
const PROXHY_BIN_NAME: &str = "proxhy-windows.exe";

// --- paths ---

fn proxhy_data_dir() -> PathBuf {
    let dir = dirs::data_dir()
        .expect("no platform data dir")
        .join("proxhy");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn proxhy_binary_path() -> PathBuf {
    proxhy_data_dir().join(PROXHY_BIN_NAME)
}

fn proxhy_version_path() -> PathBuf {
    proxhy_data_dir().join("proxhy_version.txt")
}

fn read_installed_version() -> Option<String> {
    std::fs::read_to_string(proxhy_version_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_installed_version(version: &str) {
    let _ = std::fs::write(proxhy_version_path(), version);
}

// --- log types ---

#[derive(Clone, Debug, PartialEq)]
enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
    Trace,
    Gui,
    Unknown,
}

#[derive(Clone, Debug)]
struct LogLine {
    level: LogLevel,
    text: String,
}

impl LogLine {
    fn parse(raw: &str) -> Self {
        let upper = raw.to_uppercase();
        let level = if raw.starts_with("[gui]") {
            LogLevel::Gui
        } else if upper.contains("[ERROR]")
            || upper.contains("ERROR:")
            || upper.contains("EXCEPTION")
            || upper.contains("TRACEBACK")
        {
            LogLevel::Error
        } else if upper.contains("[WARN]")
            || upper.contains("WARNING:")
            || upper.contains("DEPRECATIONWARNING")
            || upper.contains("WARNING,")
        {
            LogLevel::Warn
        } else if upper.contains("[DEBUG]") {
            LogLevel::Debug
        } else if upper.contains("[TRACE]") {
            LogLevel::Trace
        } else if upper.contains("[INFO]") {
            LogLevel::Info
        } else {
            LogLevel::Unknown
        };
        Self {
            level,
            text: raw.to_string(),
        }
    }

    fn color(&self) -> egui::Color32 {
        match self.level {
            LogLevel::Error => egui::Color32::from_rgb(255, 85, 85),
            LogLevel::Warn => egui::Color32::from_rgb(255, 184, 76),
            LogLevel::Info => egui::Color32::from_rgb(100, 220, 140),
            LogLevel::Debug => egui::Color32::from_rgb(130, 160, 255),
            LogLevel::Trace => egui::Color32::from_rgb(160, 130, 200),
            LogLevel::Gui => egui::Color32::from_rgb(100, 160, 255),
            LogLevel::Unknown => egui::Color32::from_rgb(180, 180, 180),
        }
    }

    fn badge_text(&self) -> Option<&'static str> {
        match self.level {
            LogLevel::Error => Some("ERR"),
            LogLevel::Warn => Some("WRN"),
            LogLevel::Info => Some("INF"),
            LogLevel::Debug => Some("DBG"),
            LogLevel::Trace => Some("TRC"),
            LogLevel::Gui => Some("GUI"),
            LogLevel::Unknown => None,
        }
    }
}

fn push_log(log: &Arc<Mutex<VecDeque<LogLine>>>, raw: &str, ctx: &egui::Context) {
    let mut l = log.lock().unwrap();
    if l.len() >= MAX_LOG_LINES {
        l.pop_front();
    }
    l.push_back(LogLine::parse(raw));
    drop(l);
    ctx.request_repaint();
}

// --- HTTP / download ---

fn fetch_latest_proxhy_version() -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("proxhy-gui")
        .build()
        .map_err(|e| e.to_string())?;
    let resp: serde_json::Value = client
        .get("https://api.github.com/repos/kbidlack/proxhy/releases/latest")
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    resp["tag_name"]
        .as_str()
        .ok_or_else(|| "missing tag_name in API response".to_string())
        .map(|s| s.trim_start_matches('v').to_string())
}

fn download_proxhy_binary(
    version: &str,
    log: &Arc<Mutex<VecDeque<LogLine>>>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let url = format!(
        "https://github.com/kbidlack/proxhy/releases/download/v{version}/{PROXHY_BIN_NAME}"
    );
    let dest = proxhy_binary_path();
    push_log(log, &format!("[gui] Downloading {url}..."), ctx);

    let client = reqwest::blocking::Client::builder()
        .user_agent("proxhy-gui")
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client.get(&url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }

    let total = resp.content_length();
    let tmp = dest.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 65536];
    let mut downloaded: u64 = 0;
    let mut last_bucket: Option<u64> = None;

    loop {
        let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        downloaded += n as u64;
        if let Some(t) = total {
            let pct = downloaded * 100 / t;
            let bucket = pct / 5;
            if last_bucket != Some(bucket) {
                push_log(log, &format!("[gui] {pct}% ({downloaded}/{t} bytes)"), ctx);
                last_bucket = Some(bucket);
            }
        }
    }
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    push_log(
        log,
        &format!("[gui] Download complete ({downloaded} bytes)."),
        ctx,
    );
    Ok(())
}

// --- update state ---

#[derive(Default, Clone)]
struct UpdateState {
    proxhy_available: Option<String>,
    gui_available: Option<String>,
    installing: bool,
    error: Option<String>,
}

fn gui_updater() -> Result<Box<dyn ReleaseUpdate>, self_update::errors::Error> {
    self_update::backends::github::Update::configure()
        .repo_owner("kbidlack")
        .repo_name("proxhy-gui")
        .bin_name("proxhy-gui")
        .current_version(env!("CARGO_PKG_VERSION"))
        .no_confirm(true)
        .build()
}

// --- background startup tasks ---

fn spawn_ensure_binary(
    log: Arc<Mutex<VecDeque<LogLine>>>,
    state: Arc<Mutex<UpdateState>>,
    ctx: egui::Context,
) {
    if proxhy_binary_path().exists() {
        return;
    }
    thread::spawn(move || {
        push_log(&log, "[gui] proxhy not found — downloading...", &ctx);
        state.lock().unwrap().installing = true;
        let result = fetch_latest_proxhy_version().and_then(|version| {
            download_proxhy_binary(&version, &log, &ctx)?;
            write_installed_version(&version);
            Ok(())
        });
        let mut s = state.lock().unwrap();
        s.installing = false;
        match result {
            Ok(()) => {
                drop(s);
                push_log(&log, "[gui] proxhy ready.", &ctx);
            }
            Err(e) => {
                s.error = Some(e.clone());
                drop(s);
                push_log(&log, &format!("[gui] Download failed: {e}"), &ctx);
            }
        }
    });
}

fn spawn_proxhy_update_check(state: Arc<Mutex<UpdateState>>) {
    // Skip if binary not present — first-run download handles the initial install.
    if !proxhy_binary_path().exists() {
        return;
    }
    thread::spawn(move || match fetch_latest_proxhy_version() {
        Ok(latest) => {
            if read_installed_version().as_deref() != Some(&*latest) {
                state.lock().unwrap().proxhy_available = Some(latest);
            }
        }
        Err(e) => {
            state.lock().unwrap().error = Some(format!("proxhy update check: {e}"));
        }
    });
}

fn spawn_gui_update_check(state: Arc<Mutex<UpdateState>>) {
    thread::spawn(
        move || match gui_updater().and_then(|u| u.get_latest_release()) {
            Ok(release) if release.version != env!("CARGO_PKG_VERSION") => {
                state.lock().unwrap().gui_available = Some(release.version);
            }
            Ok(_) => {}
            Err(e) => {
                state.lock().unwrap().error = Some(format!("GUI update check: {e}"));
            }
        },
    );
}

// --- update application ---

fn apply_proxhy_update(
    version: String,
    state: Arc<Mutex<UpdateState>>,
    log: Arc<Mutex<VecDeque<LogLine>>>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        state.lock().unwrap().installing = true;
        push_log(
            &log,
            &format!("[gui] Updating proxhy to {version}..."),
            &ctx,
        );
        match download_proxhy_binary(&version, &log, &ctx) {
            Ok(()) => {
                write_installed_version(&version);
                let mut s = state.lock().unwrap();
                s.proxhy_available = None;
                s.installing = false;
            }
            Err(e) => {
                push_log(&log, &format!("[gui] proxhy update failed: {e}"), &ctx);
                let mut s = state.lock().unwrap();
                s.error = Some(e);
                s.installing = false;
            }
        }
    });
}

fn apply_gui_update(
    state: Arc<Mutex<UpdateState>>,
    log: Arc<Mutex<VecDeque<LogLine>>>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        state.lock().unwrap().installing = true;
        push_log(&log, "[gui] Updating proxhy-gui...", &ctx);
        let result = gui_updater().and_then(|u| u.update());
        let mut s = state.lock().unwrap();
        s.installing = false;
        match result {
            Ok(_) => {
                s.gui_available = None;
                drop(s);
                push_log(&log, "[gui] GUI updated — please restart.", &ctx);
            }
            Err(e) => {
                s.error = Some(e.to_string());
                drop(s);
                push_log(&log, &format!("[gui] GUI update failed: {e}"), &ctx);
            }
        }
    });
}

fn apply_both_updates(
    proxhy_version: String,
    state: Arc<Mutex<UpdateState>>,
    log: Arc<Mutex<VecDeque<LogLine>>>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        state.lock().unwrap().installing = true;

        // Step 1: update proxhy binary
        push_log(
            &log,
            &format!("[gui] Updating proxhy to {proxhy_version}..."),
            &ctx,
        );
        match download_proxhy_binary(&proxhy_version, &log, &ctx) {
            Ok(()) => {
                write_installed_version(&proxhy_version);
                state.lock().unwrap().proxhy_available = None;
                push_log(&log, "[gui] proxhy updated.", &ctx);
            }
            Err(e) => {
                push_log(&log, &format!("[gui] proxhy update failed: {e}"), &ctx);
                let mut s = state.lock().unwrap();
                s.error = Some(e);
                s.installing = false;
                return;
            }
        }

        // Step 2: replace GUI binary
        push_log(&log, "[gui] Updating proxhy-gui...", &ctx);
        let result = gui_updater().and_then(|u| u.update());
        let mut s = state.lock().unwrap();
        s.installing = false;
        match result {
            Ok(_) => {
                s.gui_available = None;
                drop(s);
                push_log(&log, "[gui] GUI updated — please restart.", &ctx);
            }
            Err(e) => {
                s.error = Some(e.to_string());
                drop(s);
                push_log(&log, &format!("[gui] GUI update failed: {e}"), &ctx);
            }
        }
    });
}

// --- log filter ---

#[derive(PartialEq, Clone, Copy)]
enum LogFilter {
    All,
    InfoAndAbove,
    WarnAndAbove,
    ErrorOnly,
}

// --- app ---

struct App {
    log: Arc<Mutex<VecDeque<LogLine>>>,
    child: Option<Child>,
    auto_scroll: bool,
    filter: LogFilter,
    update_state: Arc<Mutex<UpdateState>>,
    ctx: egui::Context,
}

impl App {
    fn new(
        cc: &eframe::CreationContext,
        update_state: Arc<Mutex<UpdateState>>,
        log: Arc<Mutex<VecDeque<LogLine>>>,
    ) -> Self {
        Self {
            log,
            child: None,
            auto_scroll: true,
            filter: LogFilter::All,
            update_state,
            ctx: cc.egui_ctx.clone(),
        }
    }

    fn running(&self) -> bool {
        self.child.is_some()
    }

    fn start(&mut self) {
        let binary = proxhy_binary_path();
        if !binary.exists() {
            push_log(
                &self.log,
                "[gui] Binary not ready yet — wait for download.",
                &self.ctx,
            );
            return;
        }
        push_log(
            &self.log,
            &format!("[gui] Starting {}...", binary.display()),
            &self.ctx,
        );
        match Command::new(&binary)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(stdout) = child.stdout.take() {
                    let log = Arc::clone(&self.log);
                    let ctx = self.ctx.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                            push_log(&log, &line, &ctx);
                        }
                    });
                }
                // stderr is NOT errors — proxhy's logger writes to stderr
                if let Some(stderr) = child.stderr.take() {
                    let log = Arc::clone(&self.log);
                    let ctx = self.ctx.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                            push_log(&log, &line, &ctx);
                        }
                    });
                }
                self.child = Some(child);
            }
            Err(e) => {
                push_log(
                    &self.log,
                    &format!("[gui] Failed to start proxhy: {e}"),
                    &self.ctx,
                );
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        push_log(&self.log, "[gui] Stopped.", &self.ctx);
    }

    fn line_passes_filter(&self, line: &LogLine) -> bool {
        match self.filter {
            LogFilter::All => true,
            LogFilter::InfoAndAbove => !matches!(
                line.level,
                LogLevel::Debug | LogLevel::Trace | LogLevel::Unknown
            ),
            LogFilter::WarnAndAbove => {
                matches!(line.level, LogLevel::Warn | LogLevel::Error | LogLevel::Gui)
            }
            LogFilter::ErrorOnly => matches!(line.level, LogLevel::Error),
        }
    }

    fn poll_child(&mut self, ctx: &egui::Context) {
        if let Some(child) = &mut self.child {
            if let Ok(Some(status)) = child.try_wait() {
                push_log(&self.log, &format!("[gui] Process exited ({status})"), ctx);
                self.child = None;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn show_update_banner(&mut self, ctx: &egui::Context) {
        let state = self.update_state.lock().unwrap().clone();
        let has_proxhy = state.proxhy_available.is_some();
        let has_gui = state.gui_available.is_some();
        if !has_proxhy && !has_gui && state.error.is_none() {
            return;
        }

        egui::TopBottomPanel::top("update_banner").show(ctx, |ui| {
            ui.horizontal(|ui| {
                match (&state.proxhy_available, &state.gui_available) {
                    (Some(pv), Some(gv)) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 200, 50),
                            format!("⬆ Updates: proxhy {pv}  •  GUI {gv}"),
                        );
                        if state.installing {
                            ui.spinner();
                            ui.label("Installing...");
                        } else if ui.button("Update & Restart").clicked() {
                            let pv = pv.clone();
                            apply_both_updates(
                                pv,
                                Arc::clone(&self.update_state),
                                Arc::clone(&self.log),
                                self.ctx.clone(),
                            );
                        }
                    }
                    (Some(pv), None) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 200, 50),
                            format!("⬆ proxhy update: {pv}"),
                        );
                        if state.installing {
                            ui.spinner();
                            ui.label("Installing...");
                        } else if ui.button("Update proxhy").clicked() {
                            let pv = pv.clone();
                            apply_proxhy_update(
                                pv,
                                Arc::clone(&self.update_state),
                                Arc::clone(&self.log),
                                self.ctx.clone(),
                            );
                        }
                    }
                    (None, Some(gv)) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 200, 50),
                            format!("⬆ GUI update: {gv}"),
                        );
                        if state.installing {
                            ui.spinner();
                            ui.label("Installing...");
                        } else if ui.button("Update GUI & Restart").clicked() {
                            apply_gui_update(
                                Arc::clone(&self.update_state),
                                Arc::clone(&self.log),
                                self.ctx.clone(),
                            );
                        }
                    }
                    (None, None) => {}
                }

                if let Some(ref err) = state.error {
                    ui.colored_label(egui::Color32::RED, err);
                }
            });
        });
    }

    fn show_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("controls")
            .min_height(40.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.heading("Proxhy");
                    ui.separator();

                    if self.running() {
                        if ui.button("⏹ Stop").clicked() {
                            self.stop();
                        }
                        ui.colored_label(egui::Color32::from_rgb(100, 220, 140), "● Running");
                    } else {
                        if ui.button("▶ Start").clicked() {
                            self.start();
                        }
                        ui.colored_label(egui::Color32::GRAY, "● Stopped");
                    }

                    ui.separator();
                    ui.label("Filter:");
                    ui.selectable_value(&mut self.filter, LogFilter::All, "All");
                    ui.selectable_value(&mut self.filter, LogFilter::InfoAndAbove, "Info+");
                    ui.selectable_value(&mut self.filter, LogFilter::WarnAndAbove, "Warn+");
                    ui.selectable_value(&mut self.filter, LogFilter::ErrorOnly, "Errors");

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Clear").clicked() {
                            self.log.lock().unwrap().clear();
                        }
                        ui.checkbox(&mut self.auto_scroll, "Auto-scroll");
                    });
                });
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_child(ctx);
        self.show_update_banner(ctx);
        self.show_toolbar(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            let text_height = ui.text_style_height(&egui::TextStyle::Monospace);

            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .stick_to_bottom(self.auto_scroll)
                .show(ui, |ui| {
                    let log = self.log.lock().unwrap();
                    for line in log.iter().filter(|l| self.line_passes_filter(l)) {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;

                            if let Some(badge) = line.badge_text() {
                                let badge_color = line.color();
                                let dark_bg = egui::Color32::from_rgba_unmultiplied(
                                    badge_color.r() / 6,
                                    badge_color.g() / 6,
                                    badge_color.b() / 6,
                                    180,
                                );
                                egui::Frame::NONE
                                    .fill(dark_bg)
                                    .inner_margin(egui::Margin::symmetric(4, 1))
                                    .corner_radius(3)
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(badge)
                                                    .monospace()
                                                    .size(10.0)
                                                    .color(badge_color),
                                            )
                                            .selectable(false),
                                        );
                                    });
                            } else {
                                ui.add_space(30.0);
                            }

                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&line.text)
                                        .monospace()
                                        .size(12.0)
                                        .color(line.color()),
                                )
                                .wrap(),
                            );
                        });
                        ui.add_space(1.0);
                    }

                    ui.add_space(text_height);
                });
        });
    }
}

fn main() -> eframe::Result {
    let state = Arc::new(Mutex::new(UpdateState::default()));
    let log = Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES)));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Proxhy")
            .with_inner_size([800.0, 500.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Proxhy",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            spawn_ensure_binary(Arc::clone(&log), Arc::clone(&state), ctx);
            spawn_proxhy_update_check(Arc::clone(&state));
            spawn_gui_update_check(Arc::clone(&state));
            Ok(Box::new(App::new(cc, state, log)))
        }),
    )
}

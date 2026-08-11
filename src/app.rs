use crate::log::{LogLevel, LogLine, push_log};
#[cfg(windows)]
use crate::process::JobHandle;
use crate::process::no_window;
#[cfg(unix)]
use crate::process::{kill_tree, prepare_process_group};
use crate::update::{
    ProxhyState, UpdateState, apply_gui_update, proxhy_binary_path, run_proxhy_update,
};
use eframe::egui;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(PartialEq, Clone, Copy)]
enum LogFilter {
    All,
    InfoAndAbove,
    WarnAndAbove,
    ErrorOnly,
}

pub struct App {
    log: Arc<Mutex<VecDeque<LogLine>>>,
    child: Option<Child>,
    #[cfg(windows)]
    job: Option<JobHandle>,
    auto_scroll: bool,
    filter: LogFilter,
    update_state: Arc<Mutex<UpdateState>>,
    proxhy_state: Arc<Mutex<ProxhyState>>,
    ctx: egui::Context,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext,
        update_state: Arc<Mutex<UpdateState>>,
        proxhy_state: Arc<Mutex<ProxhyState>>,
        log: Arc<Mutex<VecDeque<LogLine>>>,
    ) -> Self {
        Self {
            log,
            child: None,
            #[cfg(windows)]
            job: None,
            auto_scroll: true,
            filter: LogFilter::All,
            update_state,
            proxhy_state,
            ctx: cc.egui_ctx.clone(),
        }
    }

    const fn running(&self) -> bool {
        self.child.is_some()
    }

    fn start(&mut self) {
        let binary = proxhy_binary_path();
        if !binary.exists() {
            push_log(
                &self.log,
                "[gui] Binary not ready yet; wait for download.",
                &self.ctx,
            );
            return;
        }
        push_log(
            &self.log,
            &format!("[gui] Starting {}...", binary.display()),
            &self.ctx,
        );
        let mut cmd = Command::new(&binary);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        no_window(&mut cmd);
        #[cfg(unix)]
        prepare_process_group(&mut cmd);
        match cmd.spawn() {
            Ok(mut child) => {
                #[cfg(windows)]
                {
                    self.job = JobHandle::new();
                    if let Some(job) = &self.job {
                        if !job.assign(&child) {
                            push_log(
                                &self.log,
                                "[gui] Warning: failed to assign proxhy to job object; \
                                 stopping may not kill all of its child processes.",
                                &self.ctx,
                            );
                        }
                    }
                }
                if let Some(stdout) = child.stdout.take() {
                    let log = Arc::clone(&self.log);
                    let ctx = self.ctx.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                            push_log(&log, &line, &ctx);
                        }
                    });
                }
                // stderr is NOT errors; proxhy's logger writes to stderr
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
        self.kill_child();
        push_log(&self.log, "[gui] Stopped.", &self.ctx);
    }

    fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            kill_tree(&child);
            #[cfg(windows)]
            if let Some(job) = self.job.take() {
                job.terminate();
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    const fn line_passes_filter(&self, line: &LogLine) -> bool {
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
        if self.proxhy_state.lock().unwrap().updating {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn show_update_banner(&self, ctx: &egui::Context) {
        let gui_state = self.update_state.lock().unwrap().clone();
        let proxhy_state = self.proxhy_state.lock().unwrap().clone();
        let proxhy_update_available = proxhy_state.update_available();
        if gui_state.gui_available.is_none()
            && gui_state.error.is_none()
            && !proxhy_update_available
            && proxhy_state.error.is_none()
        {
            return;
        }
        egui::TopBottomPanel::top("update_banner").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(ref gv) = gui_state.gui_available {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 200, 50),
                        format!("⬆ GUI update available: {gv}"),
                    );
                    if gui_state.installing {
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
                if let Some(ref err) = gui_state.error {
                    ui.colored_label(egui::Color32::RED, err);
                }
                if let (true, Some(lv)) = (
                    proxhy_update_available,
                    proxhy_state.latest_version.as_deref(),
                ) {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 200, 50),
                        format!("⬆ proxhy update available: {lv}"),
                    );
                }
                if let Some(ref err) = proxhy_state.error {
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

                    let proxhy_state = self.proxhy_state.lock().unwrap().clone();
                    let updating = proxhy_state.updating;
                    if proxhy_state.update_available()
                        && ui
                            .add_enabled(
                                !self.running() && !updating,
                                egui::Button::new("⬆ Update proxhy"),
                            )
                            .clicked()
                    {
                        run_proxhy_update(
                            Arc::clone(&self.proxhy_state),
                            Arc::clone(&self.log),
                            self.ctx.clone(),
                        );
                    }
                    if ui
                        .add_enabled(
                            !self.running() && !updating,
                            egui::Button::new("↺ Reinstall proxhy"),
                        )
                        .clicked()
                    {
                        run_proxhy_update(
                            Arc::clone(&self.proxhy_state),
                            Arc::clone(&self.log),
                            self.ctx.clone(),
                        );
                    }
                    if updating {
                        ui.spinner();
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
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.kill_child();
    }

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
                    drop(log);

                    ui.add_space(text_height);
                });
        });
    }
}

use crate::core::{self, Candidate, ScanConfig};
use gpui::{
    div, prelude::*, px, size, App, Application, Bounds, ClickEvent, Context, Div, Overflow,
    Render, SharedString, Stateful, Window, WindowBounds, WindowOptions,
};
use human_bytes::human_bytes;

struct DevstripView {
    scanning: bool,
    cleaning: bool,
    dry_run: bool,
    status_line: String,
    info_message: Option<String>,
    error_message: Option<String>,
    candidates: Vec<Candidate>,
    last_scan_config: Option<ScanConfig>,
}

impl DevstripView {
    fn new() -> Self {
        Self {
            scanning: false,
            cleaning: false,
            dry_run: true,
            status_line: "Ready to scan.".to_string(),
            info_message: Some(
                "Press Scan to analyze your workspaces. Dry run mode is enabled by default."
                    .to_string(),
            ),
            error_message: None,
            candidates: Vec::new(),
            last_scan_config: None,
        }
    }

    fn start_scan(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }

        self.scanning = true;
        self.cleaning = false;
        self.status_line = "Scanning for cleanup targets...".to_string();
        self.error_message = None;
        self.info_message = None;
        self.candidates.clear();
        cx.notify();

        let config = match Self::default_scan_config() {
            Ok(config) => config,
            Err(err) => {
                self.scanning = false;
                self.status_line = "Failed to build scan configuration.".to_string();
                self.error_message = Some(err);
                cx.notify();
                return;
            }
        };

        self.last_scan_config = Some(config.clone());

        let scan_task = cx.background_spawn(async move { core::scan(&config) });

        cx.spawn(async move |this, cx| {
            let candidates = scan_task.await;
            this.update(cx, move |this, cx| {
                this.scanning = false;
                this.candidates = candidates;
                if this.candidates.is_empty() {
                    this.status_line = "No safe cleanup targets were found.".to_string();
                    this.info_message = Some(
                        "Try adjusting the configuration or running the scan again after builds."
                            .to_string(),
                    );
                } else {
                    let total = core::scan_total_size(&this.candidates);
                    this.status_line =
                        format!("Found {} cleanup target(s).", this.candidates.len());
                    this.info_message = Some(format!(
                        "Approximate reclaimable space: {}.",
                        Self::human_readable_size(total)
                    ));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn start_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.cleaning || self.scanning {
            return;
        }
        if self.candidates.is_empty() {
            self.info_message = Some("Scan first to find cleanup targets.".to_string());
            cx.notify();
            return;
        }

        let dry_run = self.dry_run;
        let candidates = self.candidates.clone();
        self.cleaning = true;
        self.status_line = if dry_run {
            format!("Simulating cleanup of {} target(s)...", candidates.len())
        } else {
            format!("Removing {} target(s)...", candidates.len())
        };
        self.error_message = None;
        self.info_message = None;
        cx.notify();

        let cleanup_task = cx.background_spawn(async move { core::cleanup(&candidates, dry_run) });

        cx.spawn(async move |this, cx| {
            let results = cleanup_task.await;
            this.update(cx, move |this, cx| {
                this.cleaning = false;

                let mut freed = 0u64;
                let mut success_count = 0usize;
                let mut failures = Vec::new();
                let mut failure_messages = Vec::new();

                for result in results {
                    if result.success {
                        success_count += 1;
                        freed = freed.saturating_add(result.candidate.size_bytes);
                    } else {
                        failures.push(result.candidate.clone());
                        let reason = result
                            .error
                            .clone()
                            .unwrap_or_else(|| "unknown error".to_string());
                        failure_messages.push(format!(
                            "{} -> {}",
                            result.candidate.display_name(),
                            reason
                        ));
                    }
                }

                if dry_run {
                    this.status_line = format!(
                        "Dry run complete: {} target(s) would be removed ({} reclaimable).",
                        success_count,
                        Self::human_readable_size(freed)
                    );
                    this.info_message = Some(
                        "Dry run mode does not delete files. Toggle it off to perform the cleanup."
                            .to_string(),
                    );
                    this.error_message = if failure_messages.is_empty() {
                        None
                    } else {
                        Some(format!(
                            "Unable to simulate {} target(s):\n{}",
                            failure_messages.len(),
                            failure_messages.join("\n")
                        ))
                    };
                } else {
                    if failure_messages.is_empty() {
                        this.status_line = if success_count == 0 {
                            "Cleanup finished. Nothing was removed.".to_string()
                        } else {
                            format!(
                                "Cleanup finished: removed {} item(s) and reclaimed {}.",
                                success_count,
                                Self::human_readable_size(freed)
                            )
                        };
                        this.error_message = None;
                    } else {
                        this.status_line = format!(
                            "Cleanup completed with {} failure(s).",
                            failure_messages.len()
                        );
                        this.error_message = Some(format!(
                            "Failed to remove:\n{}",
                            failure_messages.join("\n")
                        ));
                    }

                    this.candidates = failures;
                    if this.candidates.is_empty() {
                        this.info_message = Some(
                            "All cleanup targets were removed. Run scan again to refresh."
                                .to_string(),
                        );
                    } else {
                        this.info_message = Some(format!(
                            "{} item(s) remain due to errors.",
                            this.candidates.len()
                        ));
                    }
                }

                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_dry_run(&mut self, cx: &mut Context<Self>) {
        self.dry_run = !self.dry_run;
        if self.dry_run {
            self.info_message =
                Some("Dry run enabled. Cleanup will only simulate deletions.".to_string());
        } else {
            self.info_message = Some("Dry run disabled. Cleanup will delete files.".to_string());
        }
        cx.notify();
    }

    fn default_scan_config() -> Result<ScanConfig, String> {
        let extra: Vec<std::path::PathBuf> = Vec::new();
        let excludes: Vec<std::path::PathBuf> = Vec::new();
        let roots = core::default_roots(&extra, &excludes)?;
        Ok(ScanConfig {
            roots,
            min_age_days: 2,
            max_depth: 5,
            keep_latest_derived: 1,
            keep_latest_cache: 1,
            exclude_paths: excludes,
        })
    }

    fn human_readable_size(bytes: u64) -> String {
        human_bytes(bytes as f64)
    }

    fn action_button<F>(
        &self,
        label: &str,
        enabled: bool,
        cx: &mut Context<Self>,
        handler: F,
    ) -> Stateful<Div>
    where
        F: Fn(&mut Self, &mut Context<Self>) + 'static,
    {
        let base = SharedString::from(format!("action-{}", label.to_lowercase().replace(' ', "-")));
        let mut button = div()
            .id(base)
            .px_4()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(gpui::rgb(0x1D4ED8))
            .text_color(gpui::white());

        if enabled {
            button = button
                .bg(gpui::rgb(0x2563EB))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _event: &ClickEvent, _, cx| {
                    handler(this, cx);
                }));
        } else {
            button = button
                .bg(gpui::rgb(0x93C5FD))
                .text_color(gpui::rgb(0x1E3A8A))
                .opacity(0.75);
        }

        button.child(label.to_string())
    }

    fn render_dry_run_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let indicator = if self.dry_run { "[x]" } else { "[ ]" };
        let (bg, border, text) = if self.dry_run {
            (
                gpui::rgb(0xECFDF5),
                gpui::rgb(0x047857),
                gpui::rgb(0x065F46),
            )
        } else {
            (
                gpui::rgb(0xF3F4F6),
                gpui::rgb(0x9CA3AF),
                gpui::rgb(0x374151),
            )
        };

        div()
            .id("dry-run-toggle")
            .flex()
            .gap_3()
            .items_center()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(bg)
            .cursor_pointer()
            .text_color(text)
            .child(
                div()
                    .border_1()
                    .border_color(border)
                    .rounded_sm()
                    .px_2()
                    .py_1()
                    .child(indicator.to_string()),
            )
            .child("Dry run (simulate cleanup)")
            .on_click(cx.listener(|this, _event: &ClickEvent, _, cx| {
                this.toggle_dry_run(cx);
            }))
    }

    fn candidate_row(index: usize, candidate: &Candidate) -> Div {
        let (background_hex, accent_hex) = Self::size_palette(candidate.size_bytes);

        let mut row = div()
            .bg(gpui::rgb(background_hex))
            .border_1()
            .border_color(gpui::rgb(0xE5E7EB))
            .rounded_lg()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2();

        let header = div()
            .flex()
            .justify_between()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0x1F2937))
                    .child(format!("#{:02} {}", index + 1, candidate.category)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(accent_hex))
                    .child(Self::human_readable_size(candidate.size_bytes)),
            );

        row = row.child(header);

        row = row.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x4B5563))
                .child(format!("Last used: {}", candidate.last_used_str())),
        );

        row = row.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x4B5563))
                .child(format!("Reason: {}", &candidate.reason)),
        );

        row.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x1F2937))
                .child(candidate.display_name()),
        )
    }

    fn size_palette(bytes: u64) -> (u32, u32) {
        if bytes >= (1u64 << 40) {
            (0xFEE2E2, 0x991B1B)
        } else if bytes >= (1u64 << 30) {
            (0xFEF3C7, 0x92400E)
        } else if bytes >= (1u64 << 20) {
            (0xDBEAFE, 0x1D4ED8)
        } else {
            (0xDCFCE7, 0x047857)
        }
    }

    fn render_roots(config: &ScanConfig) -> Stateful<Div> {
        let mut block = div()
            .id("last-scan-config")
            .flex()
            .flex_col()
            .gap_2()
            .bg(gpui::rgb(0xFFFFFF))
            .border_1()
            .border_color(gpui::rgb(0xE5E7EB))
            .rounded_md()
            .p_4();

        block = block.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x1F2937))
                .child("Last scan configuration"),
        );

        block = block.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x4B5563))
                .child("Scan roots:"),
        );

        if config.roots.is_empty() {
            block = block.child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0x4B5563))
                    .child("- current directory"),
            );
        } else {
            for root in &config.roots {
                block = block.child(
                    div()
                        .text_sm()
                        .text_color(gpui::rgb(0x4B5563))
                        .child(format!("- {}", root.display())),
                );
            }
        }

        block = block.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x4B5563))
                .child(format!(
                    "Minimum age (days): {} | Max depth: {}",
                    config.min_age_days, config.max_depth
                )),
        );

        block = block.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x4B5563))
                .child(format!(
                    "Keep latest derived: {} | Keep latest cache: {}",
                    config.keep_latest_derived, config.keep_latest_cache
                )),
        );

        block
    }

    fn info_banner(message: &str) -> Stateful<Div> {
        div()
            .id("info-banner")
            .bg(gpui::rgb(0xE0F2FE))
            .border_1()
            .border_color(gpui::rgb(0x7DD3FC))
            .rounded_md()
            .p_3()
            .text_sm()
            .text_color(gpui::rgb(0x0C4A6E))
            .child(message.to_string())
    }

    fn error_banner(message: &str) -> Stateful<Div> {
        div()
            .id("error-banner")
            .bg(gpui::rgb(0xFEE2E2))
            .border_1()
            .border_color(gpui::rgb(0xF87171))
            .rounded_md()
            .p_3()
            .text_sm()
            .text_color(gpui::rgb(0x7F1D1D))
            .child(message.to_string())
    }
}

impl Render for DevstripView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_scan = !self.scanning && !self.cleaning;
        let can_clean = !self.scanning && !self.cleaning && !self.candidates.is_empty();

        let scan_button = self.action_button("Scan", can_scan, cx, |this, cx| {
            this.start_scan(cx);
        });

        let clean_button = self.action_button("Clean", can_clean, cx, |this, cx| {
            this.start_cleanup(cx);
        });

        let mut buttons = div().flex().gap_3().flex_wrap();
        buttons = buttons.child(scan_button);
        buttons = buttons.child(clean_button);

        let dry_run_control = self.render_dry_run_toggle(cx);

        let mut control_panel = div()
            .id("control-panel")
            .flex()
            .flex_col()
            .gap_3()
            .bg(gpui::rgb(0xFFFFFF))
            .border_1()
            .border_color(gpui::rgb(0xE5E7EB))
            .rounded_md()
            .p_4();

        control_panel = control_panel.child(div().text_lg().child("Devstrip Cleaner"));
        control_panel = control_panel.child(div().text_sm().text_color(gpui::rgb(0x4B5563)).child(
            "Scan for stale build outputs and caches, then selectively clean them up.".to_string(),
        ));
        control_panel = control_panel.child(buttons);
        control_panel = control_panel.child(dry_run_control);

        let status_color = if self.cleaning || self.scanning {
            gpui::rgb(0x1D4ED8)
        } else {
            gpui::rgb(0x111827)
        };

        control_panel = control_panel.child(
            div()
                .text_sm()
                .text_color(status_color)
                .child(self.status_line.clone()),
        );

        if let Some(info) = &self.info_message {
            control_panel = control_panel.child(Self::info_banner(info));
        }

        if let Some(error) = &self.error_message {
            control_panel = control_panel.child(Self::error_banner(error));
        }

        let mut results_panel = div()
            .id("results-panel")
            .flex()
            .flex_col()
            .gap_3()
            .bg(gpui::rgb(0xF8FAFC))
            .border_1()
            .border_color(gpui::rgb(0xE5E7EB))
            .rounded_md()
            .p_4();

        results_panel = results_panel.child(div().text_lg().child("Results"));

        if let Some(config) = &self.last_scan_config {
            results_panel = results_panel.child(Self::render_roots(config));
        }

        let candidate_section = if self.last_scan_config.is_none() {
            div()
                .text_sm()
                .text_color(gpui::rgb(0x4B5563))
                .child("No scans yet. Choose Scan above to analyze your directories.".to_string())
        } else if self.scanning {
            div()
                .text_sm()
                .text_color(gpui::rgb(0x1D4ED8))
                .child("Scanning in progress...".to_string())
        } else if self.candidates.is_empty() {
            div().text_sm().text_color(gpui::rgb(0x4B5563)).child(
                "No cleanup targets available. Run a scan later to refresh results.".to_string(),
            )
        } else {
            let total = core::scan_total_size(&self.candidates);

            let summary = div()
                .text_sm()
                .text_color(gpui::rgb(0x1F2937))
                .child(format!(
                    "{} candidate(s), approx {} total.",
                    self.candidates.len(),
                    Self::human_readable_size(total)
                ));

            let mut items = div().flex().flex_col().gap_3();
            for (index, candidate) in self.candidates.iter().enumerate() {
                items = items.child(Self::candidate_row(index, candidate));
            }

            let mut scroll_container = div().flex().flex_col().gap_3().max_h(px(320.0));

            {
                let style = scroll_container.style();
                style.overflow.y = Some(Overflow::Scroll);
                style.overflow.x = Some(Overflow::Hidden);
            }

            scroll_container = scroll_container.child(items);

            div().flex().flex_col().gap_3().child(summary).child(scroll_container)
        };

        results_panel = results_panel.child(candidate_section);

        let mut layout = div().flex().flex_col().gap_4().p_4();
        layout = layout.child(control_panel);
        layout = layout.child(results_panel);

        div().size_full().bg(gpui::rgb(0xF3F4F6)).child(layout)
    }
}

pub fn run() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| DevstripView::new()),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}

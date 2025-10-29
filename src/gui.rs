use crate::core::{self, Candidate, ScanConfig};
use gpui::{
    div, prelude::*, px, size, App, Application, Bounds, ClickEvent, Context, Div, FlexDirection,
    Overflow, Render, SharedString, Stateful, Window, WindowBounds, WindowOptions,
};
use human_bytes::human_bytes;
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

struct DevstripView {
    scanning: bool,
    cleaning: bool,
    dry_run: bool,
    deep_scan: bool,
    status_line: String,
    info_message: Option<String>,
    error_message: Option<String>,
    candidates: Vec<Candidate>,
    all_candidates: Vec<Candidate>,
    available_categories: BTreeSet<String>,
    selected_categories: BTreeSet<String>,
    category_filters_dirty: bool,
    scan_cancel_flag: Option<Arc<AtomicBool>>,
    last_scan_cancelled: bool,
    show_cleanup_confirm: bool,
    last_scan_config: Option<ScanConfig>,
}

impl DevstripView {
    fn new() -> Self {
        Self {
            scanning: false,
            cleaning: false,
            dry_run: true,
            deep_scan: false,
            status_line: "Ready to scan.".to_string(),
            info_message: Some(
                "Press Scan to analyze your workspaces. Dry run mode is enabled by default."
                    .to_string(),
            ),
            error_message: None,
            candidates: Vec::new(),
            all_candidates: Vec::new(),
            available_categories: BTreeSet::new(),
            selected_categories: BTreeSet::new(),
            category_filters_dirty: false,
            scan_cancel_flag: None,
            last_scan_cancelled: false,
            show_cleanup_confirm: false,
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
        self.all_candidates.clear();
        self.available_categories.clear();
        self.scan_cancel_flag = None;
        self.last_scan_cancelled = false;
        self.show_cleanup_confirm = false;
        cx.notify();

        let config = match Self::build_scan_config(self.deep_scan) {
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

        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.scan_cancel_flag = Some(cancel_flag.clone());

        let scan_task = cx.background_spawn({
            let config = config.clone();
            let cancel_flag = cancel_flag.clone();
            async move { core::scan_with_cancel(&config, cancel_flag.as_ref()) }
        });

        cx.spawn(async move |this, cx| {
            let candidates = scan_task.await;
            this.update(cx, move |this, cx| {
                let was_cancelled = this
                    .scan_cancel_flag
                    .as_ref()
                    .map(|flag| flag.load(Ordering::Relaxed))
                    .unwrap_or(false);

                this.scanning = false;
                this.scan_cancel_flag = None;
                this.last_scan_cancelled = was_cancelled;
                this.all_candidates = candidates;
                this.sync_category_state();
                this.apply_category_filter();
                this.update_post_scan_messages(was_cancelled);
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
            if self.all_candidates.is_empty() {
                self.info_message = Some("Scan first to find cleanup targets.".to_string());
            } else {
                self.info_message = Some(
                    "No cleanup targets match the selected categories. Adjust filters or rescan."
                        .to_string(),
                );
            }
            cx.notify();
            return;
        }

        if !self.dry_run && !self.show_cleanup_confirm {
            self.show_cleanup_confirm = true;
            self.status_line = "Review cleanup confirmation.".to_string();
            self.info_message = Some(
                "Dry run is disabled. Confirm below to permanently remove selected targets."
                    .to_string(),
            );
            self.error_message = None;
            cx.notify();
            return;
        }

        self.execute_cleanup(cx);
    }

    fn execute_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.cleaning || self.scanning {
            return;
        }
        if self.candidates.is_empty() {
            return;
        }

        let dry_run = self.dry_run;
        let candidates = self.candidates.clone();
        self.show_cleanup_confirm = false;
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

                    this.all_candidates = failures;
                    this.sync_category_state();
                    this.apply_category_filter();

                    if this.all_candidates.is_empty() {
                        this.info_message = Some(
                            "All cleanup targets were removed. Run scan again to refresh."
                                .to_string(),
                        );
                    } else {
                        let visible = this.candidates.len();
                        if visible == 0 {
                            this.info_message = Some(format!(
                                "{} item(s) remain due to errors, but none match the current filters.",
                                this.all_candidates.len()
                            ));
                        } else if visible == this.all_candidates.len() {
                            this.info_message = Some(format!(
                                "{} item(s) remain due to errors.",
                                this.all_candidates.len()
                            ));
                        } else {
                            this.info_message = Some(format!(
                                "{} item(s) remain due to errors; {} match current filters.",
                                this.all_candidates.len(),
                                visible
                            ));
                        }
                    }
                }

                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn confirm_cleanup_dialog(&mut self, cx: &mut Context<Self>) {
        if self.cleaning || self.scanning {
            return;
        }
        if !self.show_cleanup_confirm {
            return;
        }
        self.execute_cleanup(cx);
    }

    fn cancel_cleanup_dialog(&mut self, cx: &mut Context<Self>) {
        if !self.show_cleanup_confirm {
            return;
        }
        self.show_cleanup_confirm = false;
        self.status_line = "Cleanup cancelled.".to_string();
        self.info_message = Some("Dry run is off. Press Clean when ready.".to_string());
        cx.notify();
    }

    fn toggle_dry_run(&mut self, cx: &mut Context<Self>) {
        self.dry_run = !self.dry_run;
        if self.dry_run {
            self.info_message =
                Some("Dry run enabled. Cleanup will only simulate deletions.".to_string());
            self.show_cleanup_confirm = false;
        } else {
            self.info_message = Some("Dry run disabled. Cleanup will delete files.".to_string());
        }
        cx.notify();
    }

    fn toggle_deep_scan(&mut self, cx: &mut Context<Self>) {
        self.deep_scan = !self.deep_scan;
        if self.deep_scan {
            self.info_message = Some(
                "Deep scan enabled. Future scans include all depths and recent items.".to_string(),
            );
        } else {
            self.info_message =
                Some("Deep scan disabled. Scans use the default depth and age limits.".to_string());
        }
        cx.notify();
    }

    fn stop_scan(&mut self, cx: &mut Context<Self>) {
        if !self.scanning {
            return;
        }

        if let Some(flag) = &self.scan_cancel_flag {
            if !flag.swap(true, Ordering::Relaxed) {
                self.status_line = "Stopping scan...".to_string();
                self.info_message = Some(
                    "Cancelling scan; partial results may appear once the operation stops."
                        .to_string(),
                );
                cx.notify();
            }
        }
    }

    fn scan_cancel_requested(&self) -> bool {
        self.scan_cancel_flag
            .as_ref()
            .map(|flag| flag.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    fn toggle_category(&mut self, category: &str, cx: &mut Context<Self>) {
        if !self.available_categories.contains(category) {
            return;
        }

        if self.selected_categories.contains(category) {
            self.selected_categories.remove(category);
        } else {
            self.selected_categories.insert(category.to_string());
        }

        self.category_filters_dirty = self.selected_categories != self.available_categories;
        self.apply_category_filter();
        if !self.scanning && !self.cleaning && self.last_scan_config.is_some() {
            self.update_post_scan_messages(self.last_scan_cancelled);
        }
        if self.show_cleanup_confirm {
            self.show_cleanup_confirm = false;
        }
        cx.notify();
    }

    fn sync_category_state(&mut self) {
        self.available_categories = self
            .all_candidates
            .iter()
            .map(|candidate| candidate.category.clone())
            .collect();

        if !self.category_filters_dirty {
            self.selected_categories = self.available_categories.clone();
        } else {
            let existing = self.selected_categories.clone();
            self.selected_categories = existing
                .into_iter()
                .filter(|category| self.available_categories.contains(category))
                .collect();
        }

        self.category_filters_dirty = self.selected_categories != self.available_categories;
    }

    fn apply_category_filter(&mut self) {
        if self.selected_categories.is_empty() && self.category_filters_dirty {
            self.candidates.clear();
            return;
        }

        if self.selected_categories.is_empty() {
            self.candidates = self.all_candidates.clone();
        } else {
            self.candidates = self
                .all_candidates
                .iter()
                .filter(|candidate| self.selected_categories.contains(&candidate.category))
                .cloned()
                .collect();
        }
    }

    fn update_post_scan_messages(&mut self, cancelled: bool) {
        let total = self.all_candidates.len();
        let visible = self.candidates.len();

        if total == 0 {
            if cancelled {
                self.status_line = "Scan cancelled.".to_string();
                self.info_message =
                    Some("Scan stopped before any cleanup targets were found.".to_string());
            } else {
                self.status_line = "No safe cleanup targets were found.".to_string();
                self.info_message = Some(
                    "Try adjusting the configuration or running the scan again after builds."
                        .to_string(),
                );
            }
            return;
        }

        if cancelled {
            if visible == total {
                self.status_line =
                    format!("Scan cancelled after finding {} cleanup target(s).", total);
            } else {
                self.status_line = format!(
                    "Scan cancelled after finding {} cleanup target(s); {} match current filters.",
                    total, visible
                );
            }

            if visible == 0 {
                self.info_message = Some(
                    "No items match the selected categories. Results are partial due to cancellation.".to_string(),
                );
            } else {
                let total_size = core::scan_total_size(&self.candidates);
                self.info_message = Some(format!(
                    "Partial results: approx {} reclaimable before cancellation.",
                    Self::human_readable_size(total_size)
                ));
            }
            return;
        }

        if visible == total {
            self.status_line = format!("Found {} cleanup target(s).", visible);
        } else {
            self.status_line = format!(
                "Found {} cleanup target(s); {} match current filters.",
                total, visible
            );
        }

        if visible == 0 {
            self.info_message = Some(
                "No items match the selected categories. Adjust filters or rescan.".to_string(),
            );
        } else {
            let total_size = core::scan_total_size(&self.candidates);
            self.info_message = Some(format!(
                "Approximate reclaimable space: {}.",
                Self::human_readable_size(total_size)
            ));
        }
    }

    fn build_scan_config(deep_scan: bool) -> Result<ScanConfig, String> {
        let extra: Vec<std::path::PathBuf> = Vec::new();
        let excludes: Vec<std::path::PathBuf> = Vec::new();
        let roots = core::default_roots(&extra, &excludes)?;
        let mut config = ScanConfig {
            roots,
            min_age_days: 2,
            max_depth: 5,
            keep_latest_derived: 1,
            keep_latest_cache: 1,
            exclude_paths: excludes,
        };

        if deep_scan {
            config.min_age_days = 0;
            config.max_depth = u32::MAX;
            config.keep_latest_derived = 0;
            config.keep_latest_cache = 0;
        }

        Ok(config)
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

    fn secondary_button<F>(
        &self,
        label: &str,
        enabled: bool,
        cx: &mut Context<Self>,
        handler: F,
    ) -> Stateful<Div>
    where
        F: Fn(&mut Self, &mut Context<Self>) + 'static,
    {
        let mut button = div()
            .id(SharedString::from(format!(
                "secondary-{}",
                label.to_lowercase().replace(' ', "-")
            )))
            .px_4()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(gpui::rgb(0x9CA3AF))
            .bg(gpui::rgb(0xF3F4F6))
            .text_color(gpui::rgb(0x111827));

        if enabled {
            button = button.cursor_pointer().on_click(cx.listener(
                move |this, _event: &ClickEvent, _, cx| {
                    handler(this, cx);
                },
            ));
        } else {
            button = button.opacity(0.6);
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

    fn render_deep_scan_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let indicator = if self.deep_scan { "[x]" } else { "[ ]" };
        let (bg, border, text) = if self.deep_scan {
            (
                gpui::rgb(0xEDE9FE),
                gpui::rgb(0x6D28D9),
                gpui::rgb(0x4C1D95),
            )
        } else {
            (
                gpui::rgb(0xF3F4F6),
                gpui::rgb(0x9CA3AF),
                gpui::rgb(0x374151),
            )
        };

        div()
            .id("deep-scan-toggle")
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
            .child("Deep scan (--all)")
            .on_click(cx.listener(|this, _event: &ClickEvent, _, cx| {
                this.toggle_deep_scan(cx);
            }))
    }

    fn render_project_link(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let link_text = "By ruzhila.cn".to_string();
        let link_url = "https://ruzhila.cn/?from=dev_strip_gui".to_string();

        div()
            .id("project-link")
            .text_sm()
            .text_color(gpui::rgb(0x1D4ED8))
            .cursor_pointer()
            .child(link_text)
            .on_click(cx.listener(move |this, _event: &ClickEvent, _, cx| {
                if let Err(err) = webbrowser::open(link_url.as_str()) {
                    this.error_message = Some(format!("Unable to open project website: {}", err));
                    cx.notify();
                }
            }))
    }

    fn render_cleanup_confirm(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let total = self.candidates.len();
        let approx = Self::human_readable_size(core::scan_total_size(&self.candidates));

        let mut dialog = div()
            .id("cleanup-confirm-dialog")
            .flex()
            .flex_col()
            .gap_3()
            .bg(gpui::rgb(0xFEF2F2))
            .border_1()
            .border_color(gpui::rgb(0xDC2626))
            .rounded_lg()
            .p_4();

        dialog = dialog.child(
            div()
                .text_lg()
                .text_color(gpui::rgb(0xB91C1C))
                .child("Confirm cleanup"),
        );

        dialog = dialog.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x7F1D1D))
                .child(format!(
                    "This will permanently delete {} target(s) and reclaim approximately {}.",
                    total, approx
                )),
        );

        dialog = dialog.child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0x991B1B))
                .child("This action cannot be undone."),
        );

        let mut button_row = div().flex().gap_3();
        button_row = button_row.child(self.action_button("Proceed", true, cx, |this, cx| {
            this.confirm_cleanup_dialog(cx);
        }));
        button_row = button_row.child(self.secondary_button("Cancel", true, cx, |this, cx| {
            this.cancel_cleanup_dialog(cx);
        }));

        dialog.child(button_row)
    }

    fn render_category_filters(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut block = div()
            .id("category-filters")
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
                .child("Category filters"),
        );

        if self.available_categories.is_empty() {
            return block.child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0x6B7280))
                    .child("Run a scan to populate categories.".to_string()),
            );
        }

        for category in &self.available_categories {
            let selected = self.selected_categories.contains(category);
            let indicator = if selected { "[x]" } else { "[ ]" };
            let (bg, border, text) = if selected {
                (
                    gpui::rgb(0xEEF2FF),
                    gpui::rgb(0x4338CA),
                    gpui::rgb(0x312E81),
                )
            } else {
                (
                    gpui::rgb(0xF9FAFB),
                    gpui::rgb(0xD1D5DB),
                    gpui::rgb(0x374151),
                )
            };

            let label = category.clone();
            let toggle_value = category.clone();
            let element_id = SharedString::from(format!(
                "category-{}",
                label
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect::<String>()
            ));

            block = block.child(
                div()
                    .id(element_id.clone())
                    .flex()
                    .gap_3()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(bg)
                    .text_color(text)
                    .cursor_pointer()
                    .child(
                        div()
                            .border_1()
                            .border_color(border)
                            .rounded_sm()
                            .px_2()
                            .py_1()
                            .child(indicator.to_string()),
                    )
                    .child(label.clone())
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _, cx| {
                        this.toggle_category(&toggle_value, cx);
                    })),
            );
        }

        if self.selected_categories.is_empty() && self.category_filters_dirty {
            block = block.child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0xDC2626))
                    .child("No categories selected; results are hidden.".to_string()),
            );
        }

        block
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
        let stop_enabled = self.scanning && !self.scan_cancel_requested();

        let scan_button = self.action_button("Scan", can_scan, cx, |this, cx| {
            this.start_scan(cx);
        });

        let stop_button = self.action_button("Stop", stop_enabled, cx, |this, cx| {
            this.stop_scan(cx);
        });

        let clean_button = self.action_button("Clean", can_clean, cx, |this, cx| {
            this.start_cleanup(cx);
        });

        let mut buttons = div().flex().gap_3().flex_wrap();
        buttons = buttons.child(scan_button);
        buttons = buttons.child(stop_button);
        buttons = buttons.child(clean_button);

        let dry_run_control = self.render_dry_run_toggle(cx);
        let deep_scan_control = self.render_deep_scan_toggle(cx);
        let category_filters = self.render_category_filters(cx);

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

        control_panel = control_panel.child(
            div()
                .text_lg()
                .child(format!("Devstrip Cleaner {}", env!("CARGO_PKG_VERSION"))),
        );
        control_panel = control_panel.child(div().text_sm().text_color(gpui::rgb(0x4B5563)).child(
            "Scan for stale build outputs and caches, then selectively clean them up.".to_string(),
        ));
        control_panel = control_panel.child(self.render_project_link(cx));
        control_panel = control_panel.child(buttons);
        control_panel = control_panel.child(dry_run_control);
        control_panel = control_panel.child(deep_scan_control);
        control_panel = control_panel.child(category_filters);
        if self.show_cleanup_confirm {
            control_panel = control_panel.child(self.render_cleanup_confirm(cx));
        }

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

        {
            let style = results_panel.style();
            style.flex_grow = Some(1.0);
            style.min_size.height = Some(px(0.0).into());
        }

        results_panel = results_panel.child(div().text_lg().child("Results"));

        if let Some(config) = &self.last_scan_config {
            results_panel = results_panel.child(Self::render_roots(config));
        }

        let mut candidate_container = div().flex().flex_col().gap_3();

        {
            let style = candidate_container.style();
            style.flex_grow = Some(1.0);
            style.min_size.height = Some(px(0.0).into());
        }

        let mut scroll_area = div().id("results-scroll").flex().flex_col().gap_3();

        {
            let style = scroll_area.style();
            style.size.height = Some(px(360.0).into());
            style.flex_grow = Some(0.0);
            style.flex_shrink = Some(0.0);
            style.overflow.y = Some(Overflow::Scroll);
            style.overflow.x = Some(Overflow::Hidden);
            style.scrollbar_width = Some(px(10.0).into());
        }

        if self.last_scan_config.is_none() {
            scroll_area =
                scroll_area.child(div().text_sm().text_color(gpui::rgb(0x4B5563)).child(
                    "No scans yet. Choose Scan above to analyze your directories.".to_string(),
                ));
        } else if self.scanning {
            let message = if self.scan_cancel_requested() {
                "Cancelling scan..."
            } else {
                "Scanning in progress..."
            };
            scroll_area = scroll_area.child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0x1D4ED8))
                    .child(message.to_string()),
            );
        } else if self.all_candidates.is_empty() {
            scroll_area = scroll_area.child(div().text_sm().text_color(gpui::rgb(0x4B5563)).child(
                "No cleanup targets available. Run a scan later to refresh results.".to_string(),
            ));
        } else if self.candidates.is_empty() {
            scroll_area = scroll_area.child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0x4B5563))
                    .child(
                        "No cleanup targets match the selected categories. Adjust the filters on the left or rescan."
                            .to_string(),
                    ),
            );
        } else {
            let visible_total = core::scan_total_size(&self.candidates);
            let visible_count = self.candidates.len();
            let overall_count = self.all_candidates.len();
            let summary_text = if visible_count == overall_count {
                format!(
                    "{} candidate(s), approx {} total.",
                    visible_count,
                    Self::human_readable_size(visible_total)
                )
            } else {
                let overall_total = core::scan_total_size(&self.all_candidates);
                format!(
                    "{} candidate(s) match current filters ({} total scanned). Visible approx {}, overall approx {}.",
                    visible_count,
                    overall_count,
                    Self::human_readable_size(visible_total),
                    Self::human_readable_size(overall_total)
                )
            };
            let summary = div()
                .text_sm()
                .text_color(gpui::rgb(0x1F2937))
                .child(summary_text);

            candidate_container = candidate_container.child(summary);

            let mut items = div().flex().flex_col().gap_3();
            for (index, candidate) in self.candidates.iter().enumerate() {
                items = items.child(Self::candidate_row(index, candidate));
            }

            scroll_area = scroll_area.child(items);
        }

        candidate_container = candidate_container.child(scroll_area);

        results_panel = results_panel.child(candidate_container);

        let mut layout = div().id("main-layout").size_full().flex().gap_4().p_4();

        {
            let style = layout.style();
            style.flex_grow = Some(1.0);
            style.min_size.height = Some(px(0.0).into());
            style.flex_direction = Some(FlexDirection::Row);
        }

        {
            let style = control_panel.style();
            style.size.width = Some(px(320.0).into());
            style.flex_shrink = Some(0.0);
        }

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
        cx.on_window_closed(|_app| {
            std::process::exit(0);
        })
        .detach();
        cx.activate(true);
    });
}

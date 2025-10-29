use crate::core::{self, Candidate, CleanupResult, ScanConfig};
use clap::Parser;
use human_bytes::human_bytes;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::{env, u32};

pub fn run() {
    if let Err(err) = real_main() {
        eprintln!("Error: {}", err);
        process::exit(1);
    }
}

type Result<T> = std::result::Result<T, String>;

#[derive(Parser, Debug)]
#[command(author, version, about = "Developer disk cleanup tool (CLI)", long_about = None)]
struct Args {
    #[arg(long = "roots", value_name = "PATH", num_args = 1..)]
    roots: Vec<PathBuf>,
    #[arg(value_name = "PATH")]
    positional_roots: Vec<PathBuf>,
    #[arg(short = 'x', long = "exclude", value_name = "PATH")]
    excludes: Vec<PathBuf>,
    #[arg(long = "min-age-days", default_value_t = 2)]
    min_age_days: u64,
    #[arg(long = "max-depth", default_value_t = 5)]
    max_depth: u32,
    #[arg(long = "keep-latest-derived", default_value_t = 1)]
    keep_latest_derived: usize,
    #[arg(long = "keep-latest-cache", default_value_t = 1)]
    keep_latest_cache: usize,
    #[arg(short = 'y', long = "yes")]
    yes: bool,
    #[arg(long = "dry-run")]
    dry_run: bool,
    #[arg(long = "no-color")]
    no_color: bool,
    #[arg(short = 'a', long = "all")]
    all: bool,
}

fn real_main() -> Result<()> {
    let args = Args::parse();
    let styler = TerminalStyler::new(args.no_color);
    let config = build_scan_config(&args)?;
    let candidates = run_with_spinner("Scanning for cleanup candidates", &styler, {
        let config = config.clone();
        move |reporter| {
            Ok(core::scan_with_callback(&config, |message| {
                reporter.update(message)
            }))
        }
    })?;

    if candidates.is_empty() {
        println!("{}", styler.warning("No safe cleanup targets were found."));
        return Ok(());
    }

    print_cli_report(&candidates, &styler);

    if args.dry_run {
        println!("{}", styler.dim("Dry-run: no files will be removed."));
        return Ok(());
    }

    if !args.yes && !confirm_cleanup(&styler)? {
        println!("Cleanup aborted.");
        return Ok(());
    }

    let results = cleanup_with_progress(&candidates, false, &styler);

    let success_count = results.iter().filter(|r| r.success).count();
    let freed: u64 = results
        .iter()
        .filter(|r| r.success)
        .map(|r| r.candidate.size_bytes)
        .sum();
    println!(
        "{}",
        styler.success(&format!(
            "Removed {} item(s); reclaimed approximately {}.",
            success_count,
            humanize_bytes(freed)
        ))
    );

    let failures: Vec<&CleanupResult> = results.iter().filter(|r| !r.success).collect();
    if !failures.is_empty() {
        println!(
            "{}",
            styler.error("Failed to remove the following targets:")
        );
        for failure in failures {
            let reason = failure.error.as_deref().unwrap_or("unknown error");
            println!("- {}: {}", failure.candidate.display_name(), reason);
        }
        return Err("One or more targets could not be removed.".to_string());
    }

    Ok(())
}

fn build_scan_config(args: &Args) -> Result<ScanConfig> {
    let mut roots = expand_paths(&args.roots);
    roots.extend(expand_paths(&args.positional_roots));

    let exclude_inputs = expand_paths(&args.excludes);
    let exclude_paths = core::normalize_paths(&exclude_inputs);
    let resolved_roots = core::default_roots(&roots, &exclude_paths)?;
    if args.all {
        Ok(ScanConfig {
            roots: resolved_roots,
            min_age_days: 0,
            max_depth: u32::MAX,
            keep_latest_derived: 0,
            keep_latest_cache: 0,
            exclude_paths,
        })
    } else {
        Ok(ScanConfig {
            roots: resolved_roots,
            min_age_days: args.min_age_days,
            max_depth: args.max_depth.max(1),
            keep_latest_derived: args.keep_latest_derived,
            keep_latest_cache: args.keep_latest_cache,
            exclude_paths,
        })
    }
}

fn expand_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw.starts_with("~/") || raw == "~" {
        if let Some(home) = core::home_dir() {
            let trimmed = raw.trim_start_matches('~');
            return home.join(trimmed.trim_start_matches('/'));
        }
    }
    PathBuf::from(raw.as_ref())
}

fn expand_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().map(|p| expand_path(p)).collect()
}

struct TerminalStyler {
    use_color: bool,
    supports_animation: bool,
}

impl TerminalStyler {
    const RESET: &'static str = "\u{1b}[0m";
    const BOLD: &'static str = "\u{1b}[1m";
    const DIM: &'static str = "\u{1b}[2m";
    const RED: &'static str = "\u{1b}[31m";
    const GREEN: &'static str = "\u{1b}[32m";
    const YELLOW: &'static str = "\u{1b}[33m";
    const BLUE: &'static str = "\u{1b}[34m";
    const CYAN: &'static str = "\u{1b}[36m";

    fn new(no_color: bool) -> Self {
        let stdout_terminal = io::stdout().is_terminal();
        let env_no_color = env::var_os("NO_COLOR").is_some();
        let use_color = !no_color && stdout_terminal && !env_no_color;
        let supports_animation = stdout_terminal;
        Self {
            use_color,
            supports_animation,
        }
    }

    fn format(&self, text: &str, codes: &[&str]) -> String {
        if !self.use_color || codes.is_empty() {
            return text.to_string();
        }
        let mut out = String::new();
        for code in codes {
            out.push_str(code);
        }
        out.push_str(text);
        out.push_str(Self::RESET);
        out
    }

    fn bold(&self, text: &str) -> String {
        self.format(text, &[Self::BOLD])
    }

    fn dim(&self, text: &str) -> String {
        self.format(text, &[Self::DIM])
    }

    fn success(&self, text: &str) -> String {
        self.format(text, &[Self::GREEN])
    }

    fn warning(&self, text: &str) -> String {
        self.format(text, &[Self::YELLOW])
    }

    fn blue(&self, text: &str) -> String {
        self.format(text, &[Self::BLUE])
    }

    fn error(&self, text: &str) -> String {
        self.format(text, &[Self::RED])
    }

    fn accent(&self, text: &str) -> String {
        self.format(text, &[Self::CYAN])
    }
}

struct StatusReporter {
    kind: ReporterKind,
}

enum ReporterKind {
    Channel(mpsc::Sender<String>),
    Print,
}

impl StatusReporter {
    fn channel(tx: mpsc::Sender<String>) -> Self {
        Self {
            kind: ReporterKind::Channel(tx),
        }
    }

    fn print() -> Self {
        Self {
            kind: ReporterKind::Print,
        }
    }

    fn update(&self, text: impl AsRef<str>) {
        match &self.kind {
            ReporterKind::Channel(tx) => {
                let _ = tx.send(text.as_ref().to_string());
            }
            ReporterKind::Print => {
                println!("{}", text.as_ref());
            }
        }
    }
}

fn run_with_spinner<T, F>(message: &str, styler: &TerminalStyler, func: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(StatusReporter) -> Result<T> + Send + 'static,
{
    if !styler.supports_animation {
        println!("{}...", message);
        let reporter = StatusReporter::print();
        let result = func(reporter)?;
        println!("{} done", message);
        return Ok(result);
    }

    let (status_tx, status_rx) = mpsc::channel::<String>();
    let (result_tx, result_rx) = mpsc::channel::<Result<T>>();
    let message_owned = message.to_string();

    thread::spawn(move || {
        let reporter = StatusReporter::channel(status_tx);
        let outcome = func(reporter);
        let _ = result_tx.send(outcome);
    });

    let mut current = message_owned;
    let frames = ["|", "/", "-", "\\"];
    let mut frame_index = 0usize;
    let mut prev_len = 0usize;

    loop {
        match status_rx.try_recv() {
            Ok(update) => current = update,
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {}
        }

        match result_rx.try_recv() {
            Ok(result) => {
                let final_text = format!("{} done", truncate_status(&current));
                let padding = " ".repeat(prev_len.saturating_sub(final_text.len()));
                print!("\r{}{}\n", final_text, padding);
                let _ = io::stdout().flush();
                return result;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                let final_text = format!("{} done", truncate_status(&current));
                let padding = " ".repeat(prev_len.saturating_sub(final_text.len()));
                print!("\r{}{}\n", final_text, padding);
                let _ = io::stdout().flush();
                return Err("Background task ended unexpectedly.".to_string());
            }
        }

        let frame = frames[frame_index % frames.len()];
        frame_index += 1;
        let truncated = truncate_status(&current);
        let text = format!("{} {}", frame, truncated);
        let padding = " ".repeat(prev_len.saturating_sub(text.len()));
        print!("\r{}{}", text, padding);
        let _ = io::stdout().flush();
        prev_len = text.len();
        thread::sleep(Duration::from_millis(100));
    }
}

fn truncate_status(text: &str) -> String {
    const LIMIT: usize = 80;
    if text.len() <= LIMIT {
        text.to_string()
    } else {
        let mut truncated = text.chars().take(LIMIT - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

fn truncate_middle(text: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_len {
        return text.to_string();
    }
    if max_len == 1 {
        return "…".to_string();
    }
    let head_len = (max_len - 1) / 2;
    let tail_len = max_len - 1 - head_len;
    let mut result = String::new();
    result.extend(chars.iter().take(head_len));
    result.push('…');
    result.extend(chars.iter().skip(chars.len() - tail_len));
    result
}

fn print_cli_report(candidates: &[Candidate], styler: &TerminalStyler) {
    let headers = [
        styler.bold("#"),
        styler.bold("Category"),
        styler.bold("Size"),
        styler.bold("Last Used"),
        styler.bold("Reason"),
        styler.bold("Path"),
    ];
    println!("{}", headers.join(" "));

    let category_width = candidates
        .iter()
        .map(|c| c.category.len())
        .max()
        .map(|w| w.max(8))
        .unwrap_or(8);
    let size_width = candidates
        .iter()
        .map(|c| humanize_bytes(c.size_bytes).len())
        .max()
        .unwrap_or(6);
    let last_width = 12usize;
    let reason_width = 48usize;

    for (idx, candidate) in candidates.iter().enumerate() {
        let size_text = humanize_bytes(candidate.size_bytes);
        let size_plain = format!("{:>width$}", size_text, width = size_width);
        let size_colored = colorize_size(candidate.size_bytes, &size_plain, styler);
        let category_text = format!("{:<width$}", candidate.category, width = category_width);
        let category_colored = styler.accent(&category_text);
        let index_label = styler.dim(&format!("[{:02}]", idx + 1));
        let last_used_plain = format!("{:<width$}", candidate.last_used_str(), width = last_width,);
        let last_used = styler.dim(&last_used_plain);
        let reason_plain = truncate_middle(&candidate.reason, reason_width);
        let reason = styler.dim(&reason_plain);
        println!(
            "{} {} {} {} {} -> {}",
            index_label,
            category_colored,
            size_colored,
            last_used,
            reason,
            candidate.display_name()
        );
    }

    let total = core::scan_total_size(candidates);
    println!(
        "{}",
        styler.bold(&format!("Reclaimable space: {}", humanize_bytes(total)))
    );
}

fn cleanup_with_progress(
    candidates: &[Candidate],
    dry_run: bool,
    styler: &TerminalStyler,
) -> Vec<CleanupResult> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let results = core::cleanup_with_callback(candidates, dry_run, |progress| {
        render_cleanup_progress(progress.index, progress.total, progress.candidate, styler);
    });

    if styler.supports_animation {
        println!();
    }

    results
}

fn render_cleanup_progress(
    index: usize,
    total: usize,
    candidate: &Candidate,
    styler: &TerminalStyler,
) {
    if styler.supports_animation {
        let bar = render_progress_bar(index + 1, total, 28);
        let label = styler.bold(&format!("[{}]", bar));
        print!(
            "\rCleaning {} {}/{} {}",
            label,
            index + 1,
            total,
            candidate.display_name()
        );
        let _ = io::stdout().flush();
    } else {
        println!(
            "Cleaning {}/{}: {}",
            index + 1,
            total,
            candidate.display_name()
        );
    }
}

fn render_progress_bar(position: usize, total: usize, width: usize) -> String {
    if total == 0 || width == 0 {
        return String::new();
    }
    let filled = ((position * width) + total - 1) / total;
    let filled = filled.min(width);
    let mut bar = String::new();
    bar.push_str(&"#".repeat(filled));
    bar.push_str(&"-".repeat(width - filled));
    bar
}

fn confirm_cleanup(styler: &TerminalStyler) -> Result<bool> {
    print!(
        "{}",
        styler.bold("Type yes to proceed with cleanup [yes/N]: ")
    );
    let _ = io::stdout().flush();
    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(_) => Ok(input.trim().eq_ignore_ascii_case("yes")),
        Err(err) => Err(format!("Failed to read input: {}", err)),
    }
}

fn humanize_bytes(size: u64) -> String {
    human_bytes(size as f64)
}

fn colorize_size(size_bytes: u64, text: &str, styler: &TerminalStyler) -> String {
    if size_bytes >= 1_u64 << 40 {
        styler.accent(text)
    } else if size_bytes >= 1_u64 << 30 {
        styler.warning(text)
    } else if size_bytes >= 1_u64 << 20 {
        styler.blue(text)
    } else if size_bytes >= 1_u64 << 10 {
        styler.success(text)
    } else {
        styler.dim(text)
    }
}

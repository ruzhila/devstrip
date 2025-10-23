use chrono::{DateTime, Local, Utc};
use clap::Parser;
use human_bytes::human_bytes;
use std::collections::{HashSet, VecDeque};
use std::{env, u32};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, String>;

const DEFAULT_HOME_PROJECT_DIRS: &[&str] = &["Projects", "workspace", "Work", "Developer"];
const SKIP_DIR_NAMES: &[&str] = &[".git", ".hg", ".svn", ".idea", ".vscode", ".gradle"];
const PROJECT_PATTERNS: &[&str] = &[
    "build",
    "dist",
    "out",
    "_build",
    "target",
    "node_modules",
    "DerivedData",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".eggs",
    "coverage",
    "__pycache__",
    ".parcel-cache",
    ".gradle",
    ".sass-cache",
    ".cache",
];
const CACHE_TARGETS: &[(&str, &str, &str)] = &[
    ("Library/Caches/pip", "Python", "pip cache"),
    (".cache/pip", "Python", "pip cache"),
    (".cache/pip-tools", "Python", "pip-tools cache"),
    (".cache/pipenv", "Python", "pipenv cache"),
    (".cache/pre-commit", "Python", "pre-commit cache"),
    (".cache/matplotlib", "Python", "matplotlib cache"),
    (".cache/pytest", "Python", "pytest cache"),
    (".cache/ruff", "Python", "ruff cache"),
    (".cache/uv", "Python", "uv cache"),
    (".npm", "Node", "npm cache"),
    ("Library/Caches/npm", "Node", "npm cache"),
    ("Library/Caches/Yarn", "Node", "Yarn cache"),
    (".cache/yarn", "Node", "Yarn cache"),
    ("Library/Caches/CocoaPods", "CocoaPods", "CocoaPods cache"),
    (".gradle/caches", "Gradle", "Gradle caches"),
    (".gradle/daemon", "Gradle", "Gradle daemons"),
    (".gradle/native", "Gradle", "Gradle native cache"),
    (
        "Library/Caches/JetBrains",
        "JetBrains",
        "JetBrains IDE caches",
    ),
    (
        "Library/Application Support/Code/Cache",
        "VSCode",
        "VSCode cache",
    ),
    (
        "Library/Application Support/Code/CachedData",
        "VSCode",
        "VSCode cached data",
    ),
    (
        "Library/Application Support/Slack/Service Worker/CacheStorage",
        "Slack",
        "Slack cache",
    ),
];

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

#[derive(Clone)]
struct ScanConfig {
    roots: Vec<PathBuf>,
    min_age_days: u64,
    max_depth: u32,
    keep_latest_derived: usize,
    keep_latest_cache: usize,
    exclude_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    size_bytes: u64,
    category: String,
    reason: String,
    last_used: Option<SystemTime>,
}

impl Candidate {
    fn display_name(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    fn last_used_str(&self) -> String {
        match self.last_used {
            Some(ts) => format_system_time(ts),
            None => "-".to_string(),
        }
    }
}

struct CleanupResult {
    candidate: Candidate,
    success: bool,
    error: Option<String>,
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

fn main() {
    if let Err(err) = real_main() {
        eprintln!("Error: {}", err);
        process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let args = Args::parse();
    let styler = TerminalStyler::new(args.no_color);
    let config = build_scan_config(&args)?;
    let candidates = run_with_spinner("Scanning for cleanup candidates", &styler, {
        let config = config;
        move |reporter| Ok(gather_candidates(&config, &reporter))
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
    let exclude_paths = normalize_paths(&exclude_inputs);
    let resolved_roots = default_roots(&roots, &exclude_paths)?;
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
        if let Some(home) = home_dir() {
            let trimmed = raw.trim_start_matches('~');
            return home.join(trimmed.trim_start_matches('/'));
        }
    }
    PathBuf::from(raw.as_ref())
}

fn expand_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().map(|p| expand_path(p)).collect()
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn normalize_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|path| match fs::canonicalize(path) {
            Ok(resolved) => resolved,
            Err(_) => path.clone(),
        })
        .collect()
}

fn default_roots(extra: &[PathBuf], excludes: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    roots.push(
        env::current_dir().map_err(|e| format!("Unable to determine current directory: {}", e))?,
    );

    if let Some(home) = home_dir() {
        for name in DEFAULT_HOME_PROJECT_DIRS {
            let candidate = home.join(name);
            if candidate.is_dir() {
                roots.push(candidate);
            }
        }
    }

    roots.extend(extra.iter().cloned());

    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let resolved = fs::canonicalize(&root).unwrap_or(root.clone());
        if seen.contains(&resolved) {
            continue;
        }
        if !resolved.exists() {
            continue;
        }
        if is_excluded(&resolved, excludes) {
            continue;
        }
        seen.insert(resolved.clone());
        unique.push(resolved);
    }

    Ok(unique)
}

fn is_excluded(path: &Path, excludes: &[PathBuf]) -> bool {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    excludes
        .iter()
        .any(|exclude| resolved == *exclude || resolved.starts_with(exclude))
}

fn safe_metadata(path: &Path) -> Option<fs::Metadata> {
    fs::symlink_metadata(path).ok()
}

fn calculate_size(path: &Path) -> u64 {
    let metadata = match safe_metadata(path) {
        Some(meta) => meta,
        None => return 0,
    };

    if !metadata.is_dir() {
        return metadata.len();
    }

    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let entry_meta = match safe_metadata(&entry_path) {
                Some(meta) => meta,
                None => continue,
            };
            if entry_meta.file_type().is_symlink() {
                continue;
            }
            if entry_meta.is_dir() {
                stack.push(entry_path);
            } else {
                total = total.saturating_add(entry_meta.len());
            }
        }
    }

    total
}

fn delete_path(path: &Path) -> io::Result<()> {
    let metadata = match safe_metadata(path) {
        Some(meta) => meta,
        None => return Ok(()),
    };
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn collect_keep_latest(
    base: &Path,
    keep: usize,
    category: &str,
    reason: &str,
    excludes: &[PathBuf],
    reporter: &StatusReporter,
) -> Vec<Candidate> {
    let mut results = Vec::new();
    if is_excluded(base, excludes) || !base.exists() {
        return results;
    }
    reporter.update(format!("Scanning: {}", base.display()));

    let entries = match fs::read_dir(base) {
        Ok(iter) => iter,
        Err(_) => return results,
    };

    let mut dated_dirs = Vec::new();
    for entry in entries.flatten() {
        let child = entry.path();
        if is_excluded(&child, excludes) {
            continue;
        }
        reporter.update(format!("Scanning: {}", child.display()));
        let metadata = match safe_metadata(&child) {
            Some(meta) => meta,
            None => continue,
        };
        if !metadata.is_dir() {
            continue;
        }
        if let Ok(modified) = metadata.modified() {
            dated_dirs.push((modified, child));
        }
    }

    dated_dirs.sort_by(|a, b| b.0.cmp(&a.0));

    for (index, (mtime, path)) in dated_dirs.into_iter().enumerate() {
        if index < keep {
            continue;
        }
        let size = calculate_size(&path);
        if size == 0 {
            continue;
        }
        results.push(Candidate {
            path,
            size_bytes: size,
            category: category.to_string(),
            reason: reason.to_string(),
            last_used: Some(mtime),
        });
    }

    results
}

fn collect_whole_directory(
    path: &Path,
    category: &str,
    reason: &str,
    excludes: &[PathBuf],
    reporter: &StatusReporter,
) -> Vec<Candidate> {
    if is_excluded(path, excludes) || !path.exists() {
        return Vec::new();
    }
    reporter.update(format!("Scanning: {}", path.display()));
    let size = calculate_size(path);
    if size == 0 {
        return Vec::new();
    }
    let metadata = safe_metadata(path);
    let last_used = metadata.and_then(|meta| meta.modified().ok());
    vec![Candidate {
        path: path.to_path_buf(),
        size_bytes: size,
        category: category.to_string(),
        reason: reason.to_string(),
        last_used,
    }]
}

fn collect_matching_dirs(
    roots: &[PathBuf],
    category: &str,
    reason: &str,
    min_age_days: u64,
    max_depth: u32,
    excludes: &[PathBuf],
    reporter: &StatusReporter,
) -> Vec<Candidate> {
    let mut results = Vec::new();
    let cutoff = if min_age_days == 0 {
        None
    } else {
        SystemTime::now().checked_sub(Duration::from_secs(min_age_days * 86_400))
    };

    let pattern_set: HashSet<&str> = PROJECT_PATTERNS.iter().copied().collect();
    let skip_dirs: HashSet<&str> = SKIP_DIR_NAMES.iter().copied().collect();

    for root in roots {
        if is_excluded(root, excludes) || !root.is_dir() {
            continue;
        }
        reporter.update(format!("Scanning: {}", root.display()));

        let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
        queue.push_back((root.clone(), 0));

        while let Some((current, depth)) = queue.pop_front() {
            if depth > max_depth {
                continue;
            }
            if is_excluded(&current, excludes) {
                continue;
            }
            reporter.update(format!("Scanning: {}", current.display()));

            let entries = match fs::read_dir(&current) {
                Ok(iter) => iter,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let file_type = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if file_type.is_symlink() {
                    continue;
                }
                if !file_type.is_dir() {
                    continue;
                }
                let path = entry.path();
                if is_excluded(&path, excludes) {
                    continue;
                }
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };

                if skip_dirs.contains(name) {
                    continue;
                }

                let metadata = match safe_metadata(&path) {
                    Some(meta) => meta,
                    None => continue,
                };
                let modified = metadata.modified().ok();

                if let Some(reason_text) =
                    classify_project_dir(name, reason, &pattern_set, cutoff, modified)
                {
                    let size = calculate_size(&path);
                    if size > 0 {
                        results.push(Candidate {
                            path: path.clone(),
                            size_bytes: size,
                            category: category.to_string(),
                            reason: reason_text,
                            last_used: modified,
                        });
                    }
                    continue;
                }

                if depth < max_depth {
                    queue.push_back((path, depth + 1));
                }
            }
        }
    }

    results
}

fn classify_project_dir(
    name: &str,
    base_reason: &str,
    pattern_set: &HashSet<&str>,
    cutoff: Option<SystemTime>,
    modified: Option<SystemTime>,
) -> Option<String> {
    if name == "__pycache__" {
        return Some(base_reason.to_string());
    }

    let matches_named_pattern = pattern_set.contains(name) || name.ends_with(".egg-info");
    if !matches_named_pattern {
        return None;
    }

    if let (Some(limit), Some(mtime)) = (cutoff, modified) {
        if mtime >= limit {
            return None;
        }
    }

    Some(format!("{} ({})", base_reason, name))
}

fn gather_candidates(config: &ScanConfig, reporter: &StatusReporter) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));

    let derived = home.join("Library/Developer/Xcode/DerivedData");
    candidates.extend(collect_keep_latest(
        &derived,
        config.keep_latest_derived,
        "Xcode",
        "Old DerivedData projects",
        &config.exclude_paths,
        reporter,
    ));

    let archives = home.join("Library/Developer/Xcode/Archives");
    candidates.extend(collect_keep_latest(
        &archives,
        config.keep_latest_derived,
        "Xcode",
        "Old Xcode archives",
        &config.exclude_paths,
        reporter,
    ));

    let core_sim = home.join("Library/Developer/CoreSimulator/Caches");
    candidates.extend(collect_whole_directory(
        &core_sim,
        "Xcode",
        "CoreSimulator caches",
        &config.exclude_paths,
        reporter,
    ));

    let brew_cache = home.join("Library/Caches/Homebrew");
    candidates.extend(collect_keep_latest(
        &brew_cache,
        config.keep_latest_cache,
        "Homebrew",
        "Homebrew download cache",
        &config.exclude_paths,
        reporter,
    ));

    for (path, category, reason) in build_cache_targets(&home) {
        candidates.extend(collect_whole_directory(
            &path,
            category,
            reason,
            &config.exclude_paths,
            reporter,
        ));
    }

    candidates.extend(collect_matching_dirs(
        &config.roots,
        "Project",
        "Stale build or cache",
        config.min_age_days,
        config.max_depth,
        &config.exclude_paths,
        reporter,
    ));

    candidates.sort_by(|a, b| match b.size_bytes.cmp(&a.size_bytes) {
        std::cmp::Ordering::Equal => match a.category.cmp(&b.category) {
            std::cmp::Ordering::Equal => a.display_name().cmp(&b.display_name()),
            other => other,
        },
        other => other,
    });

    candidates
}

fn build_cache_targets(home: &Path) -> Vec<(PathBuf, &'static str, &'static str)> {
    CACHE_TARGETS
        .iter()
        .map(|(relative, category, reason)| (home.join(relative), *category, *reason))
        .collect()
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

    let total: u64 = candidates.iter().map(|c| c.size_bytes).sum();
    println!(
        "{}",
        styler.bold(&format!("Reclaimable space: {}", humanize_bytes(total)))
    );
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
        let mut truncated = text[..LIMIT - 3].to_string();
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

fn cleanup_with_progress(
    candidates: &[Candidate],
    dry_run: bool,
    styler: &TerminalStyler,
) -> Vec<CleanupResult> {
    let total = candidates.len();
    let mut results = Vec::with_capacity(total);
    if total == 0 {
        return results;
    }

    for (index, candidate) in candidates.iter().enumerate() {
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

        let (success, error) = if dry_run {
            (true, None)
        } else {
            match delete_path(&candidate.path) {
                Ok(_) => (true, None),
                Err(err) => (false, Some(err.to_string())),
            }
        };

        results.push(CleanupResult {
            candidate: candidate.clone(),
            success,
            error,
        });
    }

    if styler.supports_animation {
        println!();
    }

    results
}

fn render_progress_bar(position: usize, total: usize, width: usize) -> String {
    if total == 0 || width == 0 {
        return "".to_string();
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

fn format_system_time(ts: SystemTime) -> String {
    if ts.duration_since(UNIX_EPOCH).is_err() {
        return "-".to_string();
    }
    let datetime: DateTime<Local> = DateTime::<Utc>::from(ts).with_timezone(&Local);
    datetime.format("%Y-%m-%d %H:%M").to_string()
}

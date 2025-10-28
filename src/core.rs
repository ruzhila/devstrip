use chrono::{DateTime, Local, Utc};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type CoreResult<T> = std::result::Result<T, String>;

pub const DEFAULT_HOME_PROJECT_DIRS: &[&str] = &["Projects", "workspace", "Work", "Developer"];
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

#[derive(Clone)]
pub struct ScanConfig {
    pub roots: Vec<PathBuf>,
    pub min_age_days: u64,
    pub max_depth: u32,
    pub keep_latest_derived: usize,
    pub keep_latest_cache: usize,
    pub exclude_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub category: String,
    pub reason: String,
    pub last_used: Option<SystemTime>,
}

impl Candidate {
    pub fn display_name(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    pub fn last_used_str(&self) -> String {
        match self.last_used {
            Some(ts) => format_system_time(ts),
            None => "-".to_string(),
        }
    }
}

pub struct CleanupResult {
    pub candidate: Candidate,
    pub success: bool,
    pub error: Option<String>,
}

pub struct CleanupProgress<'a> {
    pub index: usize,
    pub total: usize,
    pub candidate: &'a Candidate,
}

pub fn scan(config: &ScanConfig) -> Vec<Candidate> {
    scan_with_callback(config, |_| {})
}

pub fn scan_with_callback<F>(config: &ScanConfig, mut callback: F) -> Vec<Candidate>
where
    F: FnMut(&str),
{
    gather_candidates(config, &mut callback)
}

pub fn cleanup(candidates: &[Candidate], dry_run: bool) -> Vec<CleanupResult> {
    cleanup_with_callback(candidates, dry_run, |_| {})
}

pub fn cleanup_with_callback<F>(
    candidates: &[Candidate],
    dry_run: bool,
    mut callback: F,
) -> Vec<CleanupResult>
where
    F: FnMut(CleanupProgress<'_>),
{
    let total = candidates.len();
    let mut results = Vec::with_capacity(total);
    for (index, candidate) in candidates.iter().enumerate() {
        callback(CleanupProgress {
            index,
            total,
            candidate,
        });

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

    results
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn default_roots(extra: &[PathBuf], excludes: &[PathBuf]) -> CoreResult<Vec<PathBuf>> {
    let mut roots = Vec::new();
    roots.push(
        std::env::current_dir()
            .map_err(|e| format!("Unable to determine current directory: {}", e))?,
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

pub fn scan_total_size(candidates: &[Candidate]) -> u64 {
    candidates.iter().map(|c| c.size_bytes).sum()
}

fn gather_candidates<F>(config: &ScanConfig, reporter: &mut F) -> Vec<Candidate>
where
    F: FnMut(&str),
{
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

    let mut candidates = dedupe_candidates(candidates);
    candidates.sort_by(|a, b| match b.size_bytes.cmp(&a.size_bytes) {
        std::cmp::Ordering::Equal => match a.category.cmp(&b.category) {
            std::cmp::Ordering::Equal => a.display_name().cmp(&b.display_name()),
            other => other,
        },
        other => other,
    });

    candidates
}

fn collect_keep_latest<F>(
    base: &Path,
    keep: usize,
    category: &str,
    reason: &str,
    excludes: &[PathBuf],
    reporter: &mut F,
) -> Vec<Candidate>
where
    F: FnMut(&str),
{
    let mut results = Vec::new();
    if is_excluded(base, excludes) || !base.exists() {
        return results;
    }
    reporter(&format!("Scanning: {}", base.display()));

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
        reporter(&format!("Scanning: {}", child.display()));
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

fn collect_whole_directory<F>(
    path: &Path,
    category: &str,
    reason: &str,
    excludes: &[PathBuf],
    reporter: &mut F,
) -> Vec<Candidate>
where
    F: FnMut(&str),
{
    if is_excluded(path, excludes) || !path.exists() {
        return Vec::new();
    }
    reporter(&format!("Scanning: {}", path.display()));
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

fn collect_matching_dirs<F>(
    roots: &[PathBuf],
    category: &str,
    reason: &str,
    min_age_days: u64,
    max_depth: u32,
    excludes: &[PathBuf],
    reporter: &mut F,
) -> Vec<Candidate>
where
    F: FnMut(&str),
{
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
        reporter(&format!("Scanning: {}", root.display()));

        let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
        queue.push_back((root.clone(), 0));

        while let Some((current, depth)) = queue.pop_front() {
            if depth > max_depth {
                continue;
            }
            if is_excluded(&current, excludes) {
                continue;
            }
            reporter(&format!("Scanning: {}", current.display()));

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

fn dedupe_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    let mut unique = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let key = canonical_key(&candidate.path);
        if seen.insert(key) {
            unique.push(candidate);
        }
    }
    unique
}

fn canonical_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn build_cache_targets(home: &Path) -> Vec<(PathBuf, &'static str, &'static str)> {
    CACHE_TARGETS
        .iter()
        .map(|(relative, category, reason)| (home.join(relative), *category, *reason))
        .collect()
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

pub fn is_excluded(path: &Path, excludes: &[PathBuf]) -> bool {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    excludes
        .iter()
        .any(|exclude| resolved == *exclude || resolved.starts_with(exclude))
}

pub fn normalize_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|path| match fs::canonicalize(path) {
            Ok(resolved) => resolved,
            Err(_) => path.clone(),
        })
        .collect()
}

pub fn format_system_time(ts: SystemTime) -> String {
    if ts.duration_since(UNIX_EPOCH).is_err() {
        return "-".to_string();
    }
    let datetime: DateTime<Local> = DateTime::<Utc>::from(ts).with_timezone(&Local);
    datetime.format("%Y-%m-%d %H:%M").to_string()
}

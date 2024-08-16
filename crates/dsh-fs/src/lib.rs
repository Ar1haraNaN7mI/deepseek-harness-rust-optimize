//! Filesystem capability with PathGuard protecting the core kernel.

mod guard;

pub use guard::{PathDecision, PathGuard, PathGuardConfig};

use anyhow::{bail, Context, Result};
use regex::Regex;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct FsService {
    guard: PathGuard,
}

impl FsService {
    pub fn new(guard: PathGuard) -> Self {
        Self { guard }
    }

    pub fn guard(&self) -> &PathGuard {
        &self.guard
    }

    pub fn read_text(&self, path: impl AsRef<Path>) -> Result<String> {
        let path = path.as_ref();
        self.guard.check_read(path)?;
        fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
    }

    /// 1-based line offset; `limit` caps number of lines returned.
    pub fn read_range(
        &self,
        path: impl AsRef<Path>,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        let path = path.as_ref();
        self.guard.check_read(path)?;
        let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        let reader = BufReader::new(file);
        let start = offset.unwrap_or(1).max(1);
        let max_lines = limit.unwrap_or(400).max(1);
        let mut out = String::new();
        let mut taken = 0usize;
        let mut total = 0usize;
        for (idx, line) in reader.lines().enumerate() {
            total += 1;
            let line_no = idx + 1;
            if line_no < start {
                continue;
            }
            if taken >= max_lines {
                continue;
            }
            let line = line?;
            out.push_str(&format!("{line_no:>6}|{line}\n"));
            taken += 1;
        }
        if start > total && total > 0 {
            bail!("offset {start} past end ({total} lines) of {}", path.display());
        }
        if start + taken <= total {
            out.push_str(&format!(
                "… truncated; showing {taken} lines from {start}; file has {total} lines\n"
            ));
        }
        Ok(out)
    }

    pub fn write_text(&self, path: impl AsRef<Path>, content: &str) -> Result<()> {
        let path = path.as_ref();
        self.guard.check_write(path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|s| s.to_str()).unwrap_or("dsh")
        ));
        fs::write(&tmp, content).with_context(|| format!("write temp {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
        Ok(())
    }

    pub fn edit_replace(
        &self,
        path: impl AsRef<Path>,
        old: &str,
        new: &str,
        replace_all: bool,
    ) -> Result<String> {
        let path = path.as_ref();
        let original = self.read_text(path)?;
        if old.is_empty() {
            bail!("old_string must not be empty");
        }
        let count = original.matches(old).count();
        if count == 0 {
            bail!("old_string not found in {}", path.display());
        }
        if !replace_all && count > 1 {
            bail!("old_string found {count} times; set replace_all=true or make it unique");
        }
        let updated = if replace_all {
            original.replace(old, new)
        } else {
            original.replacen(old, new, 1)
        };
        self.write_text(path, &updated)?;
        Ok(format!(
            "updated {} ({} replacement{})",
            path.display(),
            if replace_all { count } else { 1 },
            if replace_all && count != 1 { "s" } else { "" }
        ))
    }

    /// Apply a simplified multi-hunk patch:
    /// ```text
    /// *** Update File: path
    /// <<<<<<< SEARCH
    /// old
    /// =======
    /// new
    /// >>>>>>> REPLACE
    /// ```
    pub fn apply_patch(&self, patch: &str) -> Result<String> {
        let mut reports = Vec::new();
        let mut current_path: Option<PathBuf> = None;
        let mut mode = PatchMode::Idle;
        let mut search = String::new();
        let mut replace = String::new();

        for raw in patch.lines() {
            let line = raw;
            if let Some(rest) = line.strip_prefix("*** Update File:") {
                flush_hunk(self, &current_path, &search, &replace, &mut reports)?;
                search.clear();
                replace.clear();
                mode = PatchMode::Idle;
                current_path = Some(PathBuf::from(rest.trim()));
                continue;
            }
            if let Some(rest) = line.strip_prefix("*** Add File:") {
                flush_hunk(self, &current_path, &search, &replace, &mut reports)?;
                search.clear();
                replace.clear();
                mode = PatchMode::Idle;
                let path = PathBuf::from(rest.trim());
                current_path = Some(path.clone());
                // Next REPLACE block becomes full file content (SEARCH empty).
                continue;
            }
            if line.trim() == "<<<<<<< SEARCH" {
                mode = PatchMode::Search;
                search.clear();
                continue;
            }
            if line.trim() == "=======" {
                mode = PatchMode::Replace;
                replace.clear();
                continue;
            }
            if line.trim() == ">>>>>>> REPLACE" {
                flush_hunk(self, &current_path, &search, &replace, &mut reports)?;
                search.clear();
                replace.clear();
                mode = PatchMode::Idle;
                continue;
            }
            match mode {
                PatchMode::Search => {
                    if !search.is_empty() {
                        search.push('\n');
                    }
                    search.push_str(line);
                }
                PatchMode::Replace => {
                    if !replace.is_empty() {
                        replace.push('\n');
                    }
                    replace.push_str(line);
                }
                PatchMode::Idle => {}
            }
        }
        flush_hunk(self, &current_path, &search, &replace, &mut reports)?;
        if reports.is_empty() {
            bail!("apply_patch: no hunks found; use *** Update File: + SEARCH/REPLACE markers");
        }
        Ok(reports.join("\n"))
    }

    pub fn list_dir(&self, path: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let path = path.as_ref();
        self.guard.check_read(path)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            entries.push(entry.path());
        }
        entries.sort();
        Ok(entries)
    }

    pub fn glob(&self, root: &Path, pattern: &str, max: usize) -> Result<Vec<PathBuf>> {
        let matcher = glob::Pattern::new(pattern).context("invalid glob pattern")?;
        let mut hits = Vec::new();
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path);
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            if matcher.matches(&rel_s) || matcher.matches(path.file_name().and_then(|s| s.to_str()).unwrap_or("")) {
                hits.push(path.to_path_buf());
                if hits.len() >= max {
                    break;
                }
            }
        }
        Ok(hits)
    }

    pub fn grep(
        &self,
        root: &Path,
        pattern: &str,
        glob_pat: Option<&str>,
        max_hits: usize,
        case_insensitive: bool,
    ) -> Result<String> {
        let re = if case_insensitive {
            Regex::new(&format!("(?i){pattern}"))?
        } else {
            Regex::new(pattern)?
        };
        let globber = glob_pat
            .map(glob::Pattern::new)
            .transpose()
            .context("invalid glob")?;
        let mut out = String::new();
        let mut hits = 0usize;
        let skip = ["target", ".git", "node_modules", ".dsh-rust/local"];
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path);
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            if skip.iter().any(|s| rel_s.contains(s)) {
                continue;
            }
            if let Some(g) = &globber {
                if !g.matches(&rel_s) {
                    continue;
                }
            }
            // Skip obvious binaries by extension
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if matches!(
                    ext.to_lowercase().as_str(),
                    "exe" | "dll" | "so" | "png" | "jpg" | "jpeg" | "gif" | "pdf" | "zip" | "wasm"
                ) {
                    continue;
                }
            }
            let Ok(file) = fs::File::open(path) else {
                continue;
            };
            let reader = BufReader::new(file);
            for (idx, line) in reader.lines().enumerate() {
                let Ok(line) = line else { continue };
                if re.is_match(&line) {
                    out.push_str(&format!("{rel_s}:{}:{line}\n", idx + 1));
                    hits += 1;
                    if hits >= max_hits {
                        out.push_str("… hit limit reached\n");
                        return Ok(out);
                    }
                }
            }
        }
        if out.is_empty() {
            out.push_str("(no matches)\n");
        }
        Ok(out)
    }
}

enum PatchMode {
    Idle,
    Search,
    Replace,
}

fn flush_hunk(
    fs: &FsService,
    path: &Option<PathBuf>,
    search: &str,
    replace: &str,
    reports: &mut Vec<String>,
) -> Result<()> {
    if search.is_empty() && replace.is_empty() {
        return Ok(());
    }
    let Some(path) = path else {
        bail!("apply_patch hunk without *** Update/Add File header");
    };
    if search.is_empty() {
        // Add / overwrite whole file
        fs.write_text(path, replace)?;
        reports.push(format!("wrote {}", path.display()));
        return Ok(());
    }
    let msg = fs.edit_replace(path, search, replace, false)?;
    reports.push(msg);
    Ok(())
}

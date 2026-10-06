//! Filesystem capability with PathGuard protecting the core kernel.

mod guard;

pub use guard::{PathDecision, PathGuard, PathGuardConfig};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
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
        let path = self.guard.resolve_path(path.as_ref());
        self.guard.check_read(&path)?;
        fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
    }

    /// 1-based line offset; `limit` caps number of lines returned.
    pub fn read_range(
        &self,
        path: impl AsRef<Path>,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        let path = self.guard.resolve_path(path.as_ref());
        self.guard.check_read(&path)?;
        let file = fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
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
            bail!(
                "offset {start} past end ({total} lines) of {}",
                path.display()
            );
        }
        if taken > 0 && start.saturating_add(taken - 1) < total {
            out.push_str(&format!(
                "… truncated; showing {taken} lines from {start}; file has {total} lines\n"
            ));
        }
        Ok(out)
    }

    pub fn write_text(&self, path: impl AsRef<Path>, content: &str) -> Result<()> {
        let path = self.guard.resolve_path(path.as_ref());
        self.guard.check_write(&path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|s| s.to_str()).unwrap_or("dsh")
        ));
        fs::write(&tmp, content).with_context(|| format!("write temp {}", tmp.display()))?;
        replace_file(&tmp, &path)?;
        Ok(())
    }

    pub fn edit_replace(
        &self,
        path: impl AsRef<Path>,
        old: &str,
        new: &str,
        replace_all: bool,
    ) -> Result<String> {
        let path = self.guard.resolve_path(path.as_ref());
        let original = self.read_text(&path)?;
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
        self.write_text(&path, &updated)?;
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
    /// Validate the patch and report the files/hunks that would change.
    ///
    /// This deliberately shares the parser and planner used by
    /// [`Self::apply_patch`], so a dry-run cannot silently accept a different
    /// patch dialect than a real apply.
    pub fn preview_patch(&self, patch: &str) -> Result<PatchPreview> {
        let plan = self.plan_patch(patch)?;
        let mut files: Vec<String> = plan
            .contents
            .keys()
            .map(|path| path.display().to_string())
            .collect();
        files.extend(plan.deletions.iter().map(|path| path.display().to_string()));
        files.sort();
        Ok(PatchPreview {
            files,
            hunks: plan.hunk_count,
            operations: plan.operations,
        })
    }

    pub fn apply_patch(&self, patch: &str) -> Result<String> {
        let plan = self.plan_patch(patch)?;
        for (path, content) in &plan.contents {
            self.write_text(path, content)?;
        }
        for path in &plan.deletions {
            self.guard.check_write(path)?;
            if path.exists() {
                fs::remove_file(path).with_context(|| format!("delete {}", path.display()))?;
            }
        }
        Ok(plan.operations.join("\n"))
    }

    fn plan_patch(&self, patch: &str) -> Result<PatchPlan> {
        let hunks = parse_patch(patch)?;
        let mut contents = BTreeMap::<PathBuf, String>::new();
        let mut deletions = BTreeSet::<PathBuf>::new();
        let mut operations = Vec::with_capacity(hunks.len());

        for hunk in &hunks {
            let path = self.guard.resolve_path(&hunk.path);
            self.guard.check_write(&path)?;
            if hunk.delete {
                if contents.contains_key(&path) {
                    bail!("cannot delete and update {} in one patch", path.display());
                }
                if !path.is_file() {
                    bail!("cannot delete missing file {}", path.display());
                }
                deletions.insert(path.clone());
                operations.push(format!("deleted {}", path.display()));
                continue;
            }
            if deletions.contains(&path) {
                bail!("cannot update deleted file {} in one patch", path.display());
            }
            let current = if let Some(content) = contents.get(&path) {
                content.clone()
            } else if hunk.search.is_empty() {
                String::new()
            } else {
                self.read_text(&path)?
            };
            let (updated, report) = apply_hunk(&path, &current, &hunk.search, &hunk.replace)?;
            contents.insert(path, updated);
            operations.push(report);
        }

        Ok(PatchPlan {
            contents,
            deletions,
            operations,
            hunk_count: hunks.len(),
        })
    }

    pub fn list_dir(&self, path: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let path = self.guard.resolve_path(path.as_ref());
        self.guard.check_read(&path)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            entries.push(entry.path());
        }
        entries.sort();
        Ok(entries)
    }

    pub fn glob(&self, root: &Path, pattern: &str, max: usize) -> Result<Vec<PathBuf>> {
        if max == 0 {
            return Ok(Vec::new());
        }
        let root = self.guard.resolve_path(root);
        let matcher = glob::Pattern::new(pattern).context("invalid glob pattern")?;
        let mut hits = Vec::new();
        self.guard.check_read(&root)?;
        for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(&root).unwrap_or(path);
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            if matcher.matches(&rel_s)
                || matcher.matches(path.file_name().and_then(|s| s.to_str()).unwrap_or(""))
            {
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
        let root = self.guard.resolve_path(root);
        self.guard.check_read(&root)?;
        for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(&root).unwrap_or(path);
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

fn replace_file(temp: &Path, destination: &Path) -> Result<()> {
    if let Err(first_error) = fs::rename(temp, destination) {
        if cfg!(windows) && destination.is_file() {
            fs::remove_file(destination)
                .with_context(|| format!("remove old file {}", destination.display()))?;
            fs::rename(temp, destination).with_context(|| {
                format!(
                    "rename temp file to {} after replace",
                    destination.display()
                )
            })?;
        } else {
            let _ = fs::remove_file(temp);
            return Err(first_error)
                .with_context(|| format!("rename temp file to {}", destination.display()));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct PatchPreview {
    pub files: Vec<String>,
    pub hunks: usize,
    pub operations: Vec<String>,
}

struct PatchPlan {
    contents: BTreeMap<PathBuf, String>,
    deletions: BTreeSet<PathBuf>,
    operations: Vec<String>,
    hunk_count: usize,
}

struct PatchHunk {
    path: PathBuf,
    search: String,
    replace: String,
    delete: bool,
}

enum PatchMode {
    Idle,
    Search,
    Replace,
}

fn parse_patch(patch: &str) -> Result<Vec<PatchHunk>> {
    let mut hunks = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut mode = PatchMode::Idle;
    let mut search = String::new();
    let mut replace = String::new();
    let mut delete = false;

    // PowerShell's default UTF-8 writer may prepend a BOM. Treat it as an
    // encoding marker rather than part of the first patch header.
    let patch = patch.strip_prefix('\u{feff}').unwrap_or(patch);
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("*** Update File:") {
            push_hunk(&mut hunks, &current_path, &search, &replace, delete)?;
            search.clear();
            replace.clear();
            mode = PatchMode::Idle;
            delete = false;
            current_path = Some(PathBuf::from(rest.trim()));
            continue;
        }
        if let Some(rest) = line.strip_prefix("*** Add File:") {
            push_hunk(&mut hunks, &current_path, &search, &replace, delete)?;
            search.clear();
            replace.clear();
            mode = PatchMode::Idle;
            delete = false;
            current_path = Some(PathBuf::from(rest.trim()));
            continue;
        }
        if let Some(rest) = line.strip_prefix("*** Delete File:") {
            push_hunk(&mut hunks, &current_path, &search, &replace, delete)?;
            search.clear();
            replace.clear();
            mode = PatchMode::Idle;
            delete = true;
            current_path = Some(PathBuf::from(rest.trim()));
            continue;
        }
        if delete && line.trim() != "" {
            bail!("delete file hunk must not contain SEARCH/REPLACE content");
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
            push_hunk(&mut hunks, &current_path, &search, &replace, delete)?;
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
    push_hunk(&mut hunks, &current_path, &search, &replace, delete)?;
    if hunks.is_empty() {
        bail!(
            "apply_patch: no hunks found; use *** Update/Add/Delete File: + SEARCH/REPLACE markers"
        );
    }
    Ok(hunks)
}

fn push_hunk(
    hunks: &mut Vec<PatchHunk>,
    path: &Option<PathBuf>,
    search: &str,
    replace: &str,
    delete: bool,
) -> Result<()> {
    if !delete && search.is_empty() && replace.is_empty() {
        return Ok(());
    }
    let Some(path) = path.clone() else {
        bail!("apply_patch hunk without *** Update/Add/Delete File header");
    };
    hunks.push(PatchHunk {
        path,
        search: search.to_string(),
        replace: replace.to_string(),
        delete,
    });
    Ok(())
}

fn apply_hunk(path: &Path, current: &str, search: &str, replace: &str) -> Result<(String, String)> {
    if search.is_empty() {
        return Ok((replace.to_string(), format!("wrote {}", path.display())));
    }
    let count = current.matches(search).count();
    if count == 0 {
        bail!("old_string not found in {}", path.display());
    }
    if count > 1 {
        bail!(
            "old_string found {count} times; make it unique before applying {}",
            path.display()
        );
    }
    Ok((
        current.replacen(search, replace, 1),
        format!("updated {} (1 replacement)", path.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dsh-fs-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn service(root: &Path) -> FsService {
        FsService::new(
            PathGuard::new(PathGuardConfig {
                workspace_root: root.to_path_buf(),
                outer_home: root.join("outer"),
                workspace_outer: PathBuf::from(".dsh-rust"),
                deny_core_writes: true,
                deny_patterns: Vec::new(),
            })
            .expect("guard"),
        )
    }

    #[test]
    fn patch_preview_and_apply_support_delete_headers() {
        let root = temp_root("delete");
        std::fs::create_dir_all(&root).expect("root");
        let path = root.join("remove.txt");
        std::fs::write(&path, "remove me\n").expect("file");
        let fs = service(&root);
        let patch = format!("*** Delete File: {}\n", path.display());
        let preview = fs.preview_patch(&patch).expect("preview");
        assert_eq!(preview.hunks, 1);
        assert_eq!(preview.files, vec![path.display().to_string()]);
        fs.apply_patch(&patch).expect("apply");
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn patch_parser_accepts_a_utf8_bom_from_windows_editors() {
        let root = temp_root("bom");
        std::fs::create_dir_all(&root).expect("root");
        let fs = service(&root);
        let patch =
            "\u{feff}*** Add File: bom.txt\n<<<<<<< SEARCH\n=======\nencoded\n>>>>>>> REPLACE\n";
        fs.apply_patch(patch).expect("apply BOM patch");
        assert_eq!(
            std::fs::read_to_string(root.join("bom.txt")).expect("read"),
            "encoded"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relative_write_is_checked_against_workspace_root() {
        let root = temp_root("relative");
        std::fs::create_dir_all(&root).expect("root");
        let fs = service(&root);
        fs.write_text(Path::new("safe.txt"), "ok").expect("write");
        assert_eq!(
            std::fs::read_to_string(root.join("safe.txt")).unwrap(),
            "ok"
        );
        let decision = fs.guard().decide_write(Path::new("crates/blocked.rs"));
        assert!(
            matches!(decision, PathDecision::Deny(_)),
            "decision={decision:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn write_text_replaces_existing_file_on_all_platforms() {
        let root = temp_root("replace");
        std::fs::create_dir_all(&root).expect("root");
        let fs = service(&root);
        fs.write_text(Path::new("value.txt"), "one").expect("first");
        fs.write_text(Path::new("value.txt"), "two")
            .expect("replace");
        assert_eq!(
            std::fs::read_to_string(root.join("value.txt")).unwrap(),
            "two"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_range_only_reports_truncation_when_lines_are_omitted() {
        let root = temp_root("read-range");
        std::fs::create_dir_all(&root).expect("root");
        let fs = service(&root);
        fs.write_text(Path::new("lines.txt"), "one\ntwo\nthree\n")
            .expect("write");

        let complete = fs
            .read_range(Path::new("lines.txt"), None, Some(10))
            .expect("complete range");
        assert!(!complete.contains("truncated"));
        let partial = fs
            .read_range(Path::new("lines.txt"), None, Some(2))
            .expect("partial range");
        assert!(partial.contains("truncated"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn glob_with_zero_limit_returns_no_matches() {
        let root = temp_root("glob-zero");
        std::fs::create_dir_all(&root).expect("root");
        let fs = service(&root);
        fs.write_text(Path::new("file.txt"), "content")
            .expect("write");
        assert!(fs.glob(Path::new("."), "*", 0).expect("glob").is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn write_through_an_outside_symlink_is_denied() {
        let root = temp_root("symlink-root");
        let outside = temp_root("symlink-outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        let link = root.join("linked");
        let symlink_result = create_directory_symlink(&outside, &link);
        if symlink_result.is_err() {
            // Windows may run without the privilege required to create a
            // directory symlink; retain the test on platforms where it is
            // available instead of turning an environment limitation into a
            // false failure.
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }
        let fs = service(&root);
        let decision = fs.guard().decide_write(Path::new("linked/new.txt"));
        assert!(
            matches!(decision, PathDecision::Deny(_)),
            "decision={decision:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }
}

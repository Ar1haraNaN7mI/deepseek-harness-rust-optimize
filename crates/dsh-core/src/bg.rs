//! Background terminal jobs (Codex `/ps` + `/stop`).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct BgJobInfo {
    pub id: u64,
    pub command: String,
    pub running: bool,
    pub started_at: Instant,
    pub output_tail: String,
}

struct BgJob {
    command: String,
    started_at: Instant,
    output: Arc<Mutex<String>>,
    child: Option<Child>,
    done: bool,
}

pub struct BgTerminals {
    next_id: AtomicU64,
    jobs: Mutex<HashMap<u64, BgJob>>,
}

impl Default for BgTerminals {
    fn default() -> Self {
        Self::new()
    }
}

impl BgTerminals {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    pub fn spawn(&self, command: &str, cwd: &std::path::Path) -> anyhow::Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let output = Arc::new(Mutex::new(String::new()));
        let output_writer = output.clone();
        let cmd = command.to_string();

        #[cfg(windows)]
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-Command", &cmd])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        #[cfg(not(windows))]
        let mut child = Command::new("bash")
            .args(["-lc", &cmd])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            if let Some(mut out) = stdout {
                let _ = out.read_to_string(&mut buf);
            }
            if let Some(mut err) = stderr {
                let mut e = String::new();
                let _ = err.read_to_string(&mut e);
                if !e.is_empty() {
                    buf.push('\n');
                    buf.push_str(&e);
                }
            }
            *output_writer.lock() = buf;
        });

        self.jobs.lock().insert(
            id,
            BgJob {
                command: cmd,
                started_at: Instant::now(),
                output,
                child: Some(child),
                done: false,
            },
        );
        Ok(id)
    }

    pub fn refresh(&self) {
        let mut jobs = self.jobs.lock();
        for job in jobs.values_mut() {
            if job.done {
                continue;
            }
            if let Some(child) = job.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        job.done = true;
                        job.child = None;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        job.done = true;
                        job.child = None;
                    }
                }
            }
        }
    }

    pub fn list(&self) -> Vec<BgJobInfo> {
        self.refresh();
        let jobs = self.jobs.lock();
        let mut out: Vec<_> = jobs
            .iter()
            .map(|(id, j)| {
                let tail = j.output.lock().clone();
                let tail: String = tail
                    .chars()
                    .rev()
                    .take(800)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                BgJobInfo {
                    id: *id,
                    command: j.command.clone(),
                    running: !j.done,
                    started_at: j.started_at,
                    output_tail: tail,
                }
            })
            .collect();
        out.sort_by_key(|j| j.id);
        out
    }

    pub fn stop_all(&self) -> usize {
        let mut jobs = self.jobs.lock();
        let mut n = 0;
        for job in jobs.values_mut() {
            if let Some(mut child) = job.child.take() {
                let _ = child.kill();
                let _ = child.wait();
                job.done = true;
                n += 1;
            }
        }
        n
    }

    pub fn stop(&self, id: u64) -> bool {
        let mut jobs = self.jobs.lock();
        if let Some(job) = jobs.get_mut(&id) {
            if let Some(mut child) = job.child.take() {
                let _ = child.kill();
                let _ = child.wait();
                job.done = true;
                return true;
            }
        }
        false
    }

    pub fn format_ps(&self) -> String {
        let list = self.list();
        if list.is_empty() {
            return "background terminals: (none)".into();
        }
        let mut lines = vec!["background terminals:".to_string()];
        for j in list {
            let state = if j.running { "RUN" } else { "DONE" };
            lines.push(format!("  #{:<3} [{state}] {}", j.id, j.command));
            if !j.output_tail.trim().is_empty() {
                for line in j
                    .output_tail
                    .lines()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    lines.push(format!("      | {line}"));
                }
            }
        }
        lines.join("\n")
    }
}

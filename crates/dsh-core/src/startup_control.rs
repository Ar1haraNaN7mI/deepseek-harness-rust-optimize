//! A one-use startup preference shared with the next interactive CLI process.

use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::Path;

const NEXT_STARTUP_FILE: &str = "startup-next.txt";
const STARTUP_LOCK_FILE: &str = ".startup-next.lock";

fn open_lock(outer_home: &Path) -> Result<fs::File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(outer_home.join(STARTUP_LOCK_FILE))
        .context("open startup preference lock")
}

/// Choose whether the next interactive CLI startup plays its animation.
///
/// The preference survives process exit and replaces any pending choice. It is
/// independent of the persistent configuration and never bootstraps a runtime.
pub fn set_next_startup(outer_home: &Path, enabled: bool) -> Result<()> {
    fs::create_dir_all(outer_home).context("create startup preference directory")?;
    let lock = open_lock(outer_home)?;
    fs2::FileExt::lock_exclusive(&lock).context("lock startup preference")?;
    let temporary = outer_home.join(format!(".startup-next-{}.tmp", uuid::Uuid::new_v4()));
    let target = outer_home.join(NEXT_STARTUP_FILE);
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .context("create temporary startup preference")?;
        file.write_all(if enabled { b"on\n" } else { b"off\n" })?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &target).context("save next startup preference")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Atomically claim and remove the pending preference, if one exists.
///
/// Call only after an interactive terminal has been successfully prepared.
/// Previews, headless commands and failed/non-terminal launches must not call
/// this function. An OS file lock serializes consumers and writers; claiming
/// the marker also removes it from the pending path before it is returned.
/// The empty lock file remains on disk; its OS lock releases on process exit.
pub fn take_next_startup(outer_home: &Path) -> Result<Option<bool>> {
    let target = outer_home.join(NEXT_STARTUP_FILE);
    if !target
        .try_exists()
        .context("check next startup preference")?
    {
        return Ok(None);
    }
    let lock = open_lock(outer_home)?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            return Ok(None);
        }
        Err(error) => return Err(error).context("lock next startup preference"),
    }
    let claimed = outer_home.join(format!(".startup-next-{}.claimed", uuid::Uuid::new_v4()));
    match fs::rename(&target, &claimed) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("claim next startup preference"),
    }
    let result = (|| -> Result<Option<bool>> {
        let value = fs::read_to_string(&claimed).context("read next startup preference")?;
        match value.trim() {
            "on" => Ok(Some(true)),
            "off" => Ok(Some(false)),
            _ => anyhow::bail!("invalid next startup preference: expected on or off"),
        }
    })();
    let _ = fs::remove_file(&claimed);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};

    struct TestHome(PathBuf);

    impl TestHome {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("dsh-startup-test-{}", uuid::Uuid::new_v4())))
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn next_startup_is_consumed_once_and_missing_home_is_untouched() {
        let home = TestHome::new();
        assert_eq!(take_next_startup(&home.0).unwrap(), None);
        assert!(!home.0.exists());
        set_next_startup(&home.0, true).unwrap();
        assert_eq!(take_next_startup(&home.0).unwrap(), Some(true));
        assert_eq!(take_next_startup(&home.0).unwrap(), None);
        set_next_startup(&home.0, false).unwrap();
        assert_eq!(take_next_startup(&home.0).unwrap(), Some(false));
        assert_eq!(fs::read_dir(&home.0).unwrap().count(), 1);
        assert!(home.0.join(STARTUP_LOCK_FILE).is_file());
    }

    #[test]
    fn last_saved_choice_replaces_pending_choice() {
        let home = TestHome::new();
        set_next_startup(&home.0, true).unwrap();
        set_next_startup(&home.0, false).unwrap();
        assert_eq!(take_next_startup(&home.0).unwrap(), Some(false));
    }

    #[test]
    fn concurrent_launches_have_exactly_one_consumer() {
        let home = TestHome::new();
        set_next_startup(&home.0, true).unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let consumers: Vec<_> = (0..8)
            .map(|_| {
                let path = home.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    take_next_startup(&path).unwrap()
                })
            })
            .collect();
        let consumed = consumers
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(Option::is_some)
            .collect::<Vec<_>>();
        assert_eq!(consumed, vec![Some(true)]);
    }
}

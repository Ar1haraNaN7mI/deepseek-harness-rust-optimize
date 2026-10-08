//! Optional local web/desktop access lock. The terminal never reads this file.
use anyhow::{Context, Result};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FILE: &str = "web-access.json";
const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Deserialize, Serialize)]
struct SavedAccess {
    version: u8,
    password_hash: Option<String>,
}

/// Missing settings preserve the existing password-free behavior. Corrupt
/// settings are an error, never interpreted as permission to bypass the lock.
pub fn load(home: &Path) -> Result<Option<String>> {
    let path = home.join(FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let lock = open_lock(home)?;
    fs2::FileExt::lock_shared(&lock)?;
    let bytes = fs::read(path).context("read local web access settings")?;
    anyhow::ensure!(bytes.len() <= 4096, "invalid local web access settings");
    let saved: SavedAccess =
        serde_json::from_slice(&bytes).context("invalid local web access settings")?;
    anyhow::ensure!(saved.version == 1, "unsupported local web access settings");
    if let Some(hash) = &saved.password_hash {
        let parsed = PasswordHash::new(hash)
            .map_err(|_| anyhow::anyhow!("invalid local web password hash"))?;
        anyhow::ensure!(
            parsed.algorithm.as_str() == "argon2id",
            "unsupported local web password hash"
        );
    }
    Ok(saved.password_hash)
}

fn open_lock(home: &Path) -> Result<fs::File> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join(".web-access.lock"))?)
}

pub fn validate_password(password: &str) -> Result<()> {
    anyhow::ensure!(
        (8..=128).contains(&password.chars().count()) && !password.chars().any(char::is_control),
        "密码须为 8–128 个字符，不能包含控制字符"
    );
    Ok(())
}

pub fn hash(password: &str) -> Result<String> {
    validate_password(password)?;
    // UUID v4 is OS-random; two UUIDs provide more than the recommended salt entropy.
    let mut random = Vec::with_capacity(32);
    random.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    random.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let salt = SaltString::encode_b64(&random)
        .map_err(|_| anyhow::anyhow!("could not generate local password salt"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| anyhow::anyhow!("could not hash local web password"))
}

pub fn verify(password: &str, hash: &str) -> bool {
    if password.len() > 512 {
        return false;
    }
    PasswordHash::new(hash).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

/// Compare-and-replace under the same cross-process lock used by readers.
pub fn save(home: &Path, expected: Option<&str>, replacement: Option<String>) -> Result<()> {
    fs::create_dir_all(home)?;
    let lock = open_lock(home)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let previous = match fs::read(home.join(FILE)) {
        Ok(bytes) => serde_json::from_slice::<SavedAccess>(&bytes)?.password_hash,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        previous.as_deref() == expected,
        "访问设置已在其他窗口更新，请重新解锁后重试"
    );
    private_write(
        &home.join(FILE),
        &serde_json::to_vec(&SavedAccess {
            version: 1,
            password_hash: replacement,
        })?,
    )
}

pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[derive(Default)]
pub struct Sessions {
    fingerprint: Option<String>,
    entries: HashMap<String, Instant>,
    pub retry_after: Option<Instant>,
}

impl Sessions {
    pub fn sync(&mut self, hash: Option<&str>) {
        if self.fingerprint.as_deref() != hash {
            self.entries.clear();
            self.fingerprint = hash.map(str::to_owned);
        }
        self.entries.retain(|_, expiry| *expiry > Instant::now());
    }
    pub fn unlocked(&mut self, hash: Option<&str>, cookie: Option<&str>) -> bool {
        self.sync(hash);
        hash.is_none() || cookie.is_some_and(|value| self.entries.contains_key(value))
    }
    pub fn issue(&mut self, hash: Option<&str>) -> String {
        self.sync(hash);
        // Bound abandoned sessions without persisting a reusable browser secret.
        if self.entries.len() >= 64 {
            self.entries.clear();
        }
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        self.entries
            .insert(token.clone(), Instant::now() + SESSION_LIFETIME);
        self.retry_after = None;
        token
    }
    pub fn revoke(&mut self, cookie: Option<&str>) {
        if let Some(cookie) = cookie {
            self.entries.remove(cookie);
        }
    }
    pub fn cooling_down(&self) -> bool {
        self.retry_after
            .is_some_and(|deadline| deadline > Instant::now())
    }
    pub fn failure(&mut self) {
        self.retry_after = Some(Instant::now() + Duration::from_secs(1));
    }
}

// CLI service control uses a separate OS-local secret, never returned by any
// web API. It can still reopen/restart a locked service without a terminal login.
#[derive(Serialize, Deserialize)]
pub struct ServiceControl {
    pub instance_id: String,
    pub token: String,
}
pub fn control_path(authority: &str) -> Result<PathBuf> {
    let port: u16 = authority
        .strip_prefix("127.0.0.1:")
        .context("invalid local service authority")?
        .parse()?;
    Ok(dirs::data_local_dir()
        .context("local application data is unavailable")?
        .join("dsh-rust/web-services")
        .join(format!("{port}.json")))
}
pub fn read_control(authority: &str, instance_id: &str) -> Result<String> {
    let control: ServiceControl = serde_json::from_slice(
        &fs::read(control_path(authority)?).context("无法读取本机 Harness 控制凭据")?,
    )?;
    anyhow::ensure!(control.instance_id == instance_id, "Harness 服务已发生变化");
    Ok(control.token)
}

pub struct ControlRegistration {
    path: PathBuf,
    instance_id: String,
}
impl ControlRegistration {
    pub fn create(authority: &str, instance_id: &str, token: &str) -> Result<Self> {
        let path = control_path(authority)?;
        fs::create_dir_all(path.parent().unwrap())?;
        private_write(
            &path,
            &serde_json::to_vec(&ServiceControl {
                instance_id: instance_id.into(),
                token: token.into(),
            })?,
        )?;
        Ok(Self {
            path,
            instance_id: instance_id.into(),
        })
    }
}
impl Drop for ControlRegistration {
    fn drop(&mut self) {
        let matches = fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ServiceControl>(&bytes).ok())
            .is_some_and(|saved| saved.instance_id == self.instance_id);
        if matches {
            let _ = fs::remove_file(&self.path);
        }
    }
}

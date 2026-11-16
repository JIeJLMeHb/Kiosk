use std::time::{Duration, Instant};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::{rand_core::OsRng, SaltString};
use zeroize::Zeroize;

pub fn hash(mut pw: String) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let out = Argon2::default().hash_password(pw.as_bytes(), &salt)?.to_string();
    pw.zeroize();
    Ok(out)
}
pub fn verify(mut pw: String, phc: &str) -> bool {
    let ok = PasswordHash::new(phc)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false);
    pw.zeroize();
    ok
}

/// Экспоненциальная задержка после неверных попыток: 1,2,4,8… до 60 с.
#[derive(Default)]
pub struct Gate { attempts: u32, until: Option<Instant> }
impl Gate {
    pub fn wait_secs(&self) -> u64 {
        self.until.map(|t| t.saturating_duration_since(Instant::now()).as_secs()).unwrap_or(0)
    }
    pub fn allowed(&self) -> bool { self.wait_secs() == 0 }
    pub fn fail(&mut self) {
        self.attempts += 1;
        let s = (1u64 << self.attempts.min(6)).min(60);
        self.until = Some(Instant::now() + Duration::from_secs(s));
        tracing::warn!("неверный пароль, попытка {}", self.attempts);
    }
    pub fn ok(&mut self) { *self = Self::default(); }
}

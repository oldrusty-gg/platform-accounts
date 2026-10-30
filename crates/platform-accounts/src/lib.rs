//! Platform account foundation (future platform account service).
//!
//! Today it extends the Roses friends server's account database in place:
//! its tables are `platform_*` tables beside the server's `users` table,
//! keyed by that table's user id, with no foreign key and no dependency on
//! the server's crate, so the same code (and a copy of the rows) moves to
//! the platform service unchanged. See
//! `work/research/platform/PLATFORM-PLAN.md`.
//!
//! - [`store::PlatformStore`]: email addresses (verified or pending),
//!   account states, verification and password-reset codes (HMAC-hashed,
//!   expiring, single-use, attempt-limited), signed links, supporter flags,
//!   linked Ko-fi/Patreon ids, export and erasure.
//! - [`email`]: the [`email::EmailSender`] trait, [`email::LogSender`]
//!   (writes mail to a local file: development and tests),
//!   [`email::HttpApiSender`] (builds a provider's HTTP request; sending is
//!   switched off until a provider is chosen), message templates and a
//!   background [`email::Mailer`].
//! - [`webhooks`]: Ko-fi webhook parsing into supporter updates.

pub mod codes;
pub mod email;
pub mod store;
pub mod webhooks;

pub use store::{AccountState, Issued, PlatformStore, Problem, Profile, Purpose};

/// Seconds since 1970 (UTC).
pub fn now_seconds() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// `N` bytes from the OS CSPRNG.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    use ring::rand::SecureRandom;
    let mut out = [0u8; N];
    ring::rand::SystemRandom::new().fill(&mut out).expect("the operating system's random source failed");
    out
}

/// The server secret that keys code hashes and signed links: 32 random
/// bytes in `path`, created (owner-only where the OS allows) when missing.
pub fn load_or_create_secret(path: &std::path::Path) -> Result<[u8; 32], String> {
    match std::fs::read(path) {
        Ok(bytes) => bytes.try_into().map_err(|_| format!("{} is not a 32-byte secret", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let secret: [u8; 32] = random_bytes();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            use std::io::Write;
            let mut file = options.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
            file.write_all(&secret).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(secret)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

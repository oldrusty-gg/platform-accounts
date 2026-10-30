//! The platform's own account rows, beside (not inside) a game server's
//! user table: one `platform_users` row per account id with the email
//! address and the account's state, codes for email verification and
//! password reset, supporter flags and linked Ko-fi/Patreon ids.
//!
//! Nothing here refers to another crate's tables except the one-way
//! [`PlatformStore::migrate_legacy_users`], which copies ids from a
//! friends-server `users` table when the store shares its database file.

use crate::codes::{self, Keys};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

/// The platform tables' schema version (kept in `platform_meta`, not in
/// SQLite's `user_version`, which belongs to the database's first owner).
pub const SCHEMA: i64 = 1;
/// An email verification code lives this long.
pub const VERIFY_SECONDS: i64 = 24 * 3600;
/// A password-reset code lives this long.
pub const RESET_SECONDS: i64 = 30 * 60;
/// Wrong codes allowed before the outstanding codes of that kind stop
/// working (a new email must be asked for).
pub const MAX_TRIES: i64 = 5;
/// Shortest time between two emails of one kind to one account.
pub const RESEND_SECONDS: i64 = 60;
/// Emails of one kind to one account per day.
pub const EMAILS_PER_DAY: i64 = 5;

/// Supporter perks: platform-level and cosmetic only, never game content
/// (see the plan's legal note).
pub mod perks {
    /// A badge beside the name in the launcher and lobbies.
    pub const BADGE: u32 = 1;
    /// A profile colour/flair in the launcher.
    pub const FLAIR: u32 = 2;
    /// Early launcher builds (the platform's own software).
    pub const EARLY_LAUNCHER: u32 = 4;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    VerifyEmail,
    ResetPassword,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifyEmail => "verify_email",
            Self::ResetPassword => "reset_password",
        }
    }
    fn lifetime(self) -> i64 {
        match self {
            Self::VerifyEmail => VERIFY_SECONDS,
            Self::ResetPassword => RESET_SECONDS,
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "verify_email" => Some(Self::VerifyEmail),
            "reset_password" => Some(Self::ResetPassword),
            _ => None,
        }
    }
}

/// Where an account stands with its email address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    /// No verified address and none waiting (accounts from before email).
    NoEmail,
    /// An address was given and its code not yet entered (no verified one).
    Unverified,
    Verified,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub user_id: i64,
    pub state: AccountState,
    /// The verified address.
    pub email: Option<String>,
    /// An address waiting for its code.
    pub pending_email: Option<String>,
    pub email_verified_at: Option<i64>,
    pub display_name: Option<String>,
    pub roles: Vec<String>,
    /// OR of the active supporter flags ([`perks`]).
    pub supporter_flags: u32,
    /// The launcher should ask for an address (none yet, not dismissed).
    pub prompt_email: bool,
}

/// A code just made, to be sent by email.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issued {
    pub purpose: Purpose,
    pub user_id: i64,
    pub email: String,
    /// As shown: `ABCD-EFGH`.
    pub code: String,
    /// A signed link token (for a web page; `codes::Keys::open_link`).
    pub link: String,
    pub expires: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    BadEmail,
    /// Another account has verified this address.
    EmailTaken,
    AlreadyVerified,
    /// Wait a minute before asking again.
    TooSoon,
    /// The day's emails for this account are used up.
    TooManyEmails,
    BadCode,
    /// No code is waiting (none asked for, expired or used).
    NoCode,
    /// Too many wrong codes: ask for a new email.
    TooManyTries,
    /// Password reset needs a verified address.
    NoVerifiedEmail,
    Disabled,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadEmail => "that is not an email address",
            Self::EmailTaken => "another account uses that email address",
            Self::AlreadyVerified => "that address is already verified",
            Self::TooSoon => "an email was just sent; wait a minute",
            Self::TooManyEmails => "too many emails today; try tomorrow",
            Self::BadCode => "that code is not right",
            Self::NoCode => "no code is waiting; ask for a new email",
            Self::TooManyTries => "too many wrong codes; ask for a new email",
            Self::NoVerifiedEmail => "this account has no verified email address",
            Self::Disabled => "this account is disabled",
        })
    }
}

/// A plausible address (the real check is the code arriving): 3..=254
/// characters, one `@`, a dotted domain, no spaces or controls.
pub fn check_email(email: &str) -> Result<(), Problem> {
    let (local, domain) = email.split_once('@').ok_or(Problem::BadEmail)?;
    let ok = (3..=254).contains(&email.len())
        && (1..=64).contains(&local.len())
        && !domain.contains('@')
        && email.chars().all(|c| c.is_ascii_graphic() || (!c.is_ascii() && !c.is_control() && !c.is_whitespace()))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !email.contains("..")
        && domain.contains('.')
        && domain.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-'))
        && domain.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.');
    if ok { Ok(()) } else { Err(Problem::BadEmail) }
}

/// The comparison key of an address (unique ignoring case).
pub fn email_key(email: &str) -> String {
    email.trim().to_lowercase()
}

pub struct PlatformStore {
    db: Connection,
    keys: Keys,
}

type R<T> = Result<T, String>;
fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

impl PlatformStore {
    /// Open the platform tables in `path` (creating them; existing users
    /// of a shared friends-server database are copied in as [`AccountState::NoEmail`]).
    pub fn open(path: &Path, secret: &[u8; 32]) -> R<Self> {
        let db = Connection::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
        let _ = db.pragma_update(None, "journal_mode", "WAL");
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(e)?;
        Self::init(db, secret)
    }

    /// A database in memory (tests).
    pub fn in_memory(secret: &[u8; 32]) -> R<Self> {
        Self::init(Connection::open_in_memory().map_err(e)?, secret)
    }

    /// Use an open connection (a test database that already has a legacy
    /// `users` table).
    pub fn from_connection(db: Connection, secret: &[u8; 32]) -> R<Self> {
        Self::init(db, secret)
    }

    fn init(db: Connection, secret: &[u8; 32]) -> R<Self> {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS platform_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS platform_users(
                user_id INTEGER PRIMARY KEY,
                email TEXT,
                email_key TEXT,
                email_verified_at INTEGER,
                display_name TEXT,
                roles TEXT NOT NULL DEFAULT '',
                disabled INTEGER NOT NULL DEFAULT 0,
                email_prompt_dismissed INTEGER NOT NULL DEFAULT 0,
                created INTEGER NOT NULL,
                updated INTEGER NOT NULL);
             CREATE UNIQUE INDEX IF NOT EXISTS platform_users_email ON platform_users(email_key) WHERE email_key IS NOT NULL;
             CREATE TABLE IF NOT EXISTS platform_tokens(
                id TEXT PRIMARY KEY,
                purpose TEXT NOT NULL,
                user_id INTEGER NOT NULL,
                email TEXT NOT NULL,
                code_hash TEXT NOT NULL,
                created INTEGER NOT NULL,
                expires INTEGER NOT NULL,
                tries INTEGER NOT NULL DEFAULT 0,
                used INTEGER);
             CREATE INDEX IF NOT EXISTS platform_tokens_user ON platform_tokens(user_id, purpose);
             CREATE TABLE IF NOT EXISTS platform_supporters(
                user_id INTEGER NOT NULL,
                source TEXT NOT NULL,
                external_id TEXT,
                tier TEXT,
                flags INTEGER NOT NULL,
                since INTEGER NOT NULL,
                until INTEGER,
                PRIMARY KEY(user_id, source));
             CREATE TABLE IF NOT EXISTS platform_links(
                provider TEXT NOT NULL,
                external_id TEXT NOT NULL,
                user_id INTEGER NOT NULL,
                linked INTEGER NOT NULL,
                PRIMARY KEY(provider, external_id));
             CREATE TABLE IF NOT EXISTS platform_webhook_events(
                provider TEXT NOT NULL,
                event_id TEXT NOT NULL,
                received INTEGER NOT NULL,
                PRIMARY KEY(provider, event_id));",
        )
        .map_err(e)?;
        let version: Option<String> =
            db.query_row("SELECT value FROM platform_meta WHERE key = 'schema'", [], |r| r.get(0)).optional().map_err(e)?;
        match version.and_then(|v| v.parse::<i64>().ok()) {
            Some(v) if v > SCHEMA => return Err(format!("the platform tables are from a newer server (schema {v})")),
            _ => {
                db.execute("INSERT OR REPLACE INTO platform_meta(key, value) VALUES ('schema', ?1)", params![SCHEMA.to_string()])
                    .map_err(e)?;
            }
        }
        let store = Self { db, keys: Keys::new(secret) };
        store.migrate_legacy_users()?;
        Ok(store)
    }

    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    /// Copy every account of a friends-server `users` table in the same
    /// database that has no platform row yet (email none, so the launcher
    /// asks for one). Returns how many were added; 0 without such a table.
    pub fn migrate_legacy_users(&self) -> R<usize> {
        let has_users: bool = self
            .db
            .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'users'", [], |r| r.get::<_, i64>(0))
            .map_err(e)?
            > 0;
        if !has_users {
            return Ok(0);
        }
        self.db
            .execute(
                "INSERT OR IGNORE INTO platform_users(user_id, display_name, disabled, created, updated)
                 SELECT id, name, disabled, created, created FROM users",
                [],
            )
            .map_err(e)
    }

    /// Remove the platform rows of accounts that are gone from a shared
    /// friends-server `users` table (deleted there: their address and
    /// supporter records go too). Returns the accounts erased; 0 without
    /// such a table.
    pub fn prune_legacy_orphans(&mut self) -> R<usize> {
        let has_users: bool = self
            .db
            .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'users'", [], |r| r.get::<_, i64>(0))
            .map_err(e)?
            > 0;
        if !has_users {
            return Ok(0);
        }
        let ids: Vec<i64> = {
            let mut st = self
                .db
                .prepare(
                    "SELECT user_id FROM platform_users WHERE user_id NOT IN (SELECT id FROM users)
                     UNION SELECT user_id FROM platform_tokens WHERE user_id NOT IN (SELECT id FROM users)
                     UNION SELECT user_id FROM platform_supporters WHERE user_id NOT IN (SELECT id FROM users)
                     UNION SELECT user_id FROM platform_links WHERE user_id NOT IN (SELECT id FROM users)",
                )
                .map_err(e)?;
            let rows = st.query_map([], |r| r.get(0)).map_err(e)?;
            rows.collect::<Result<_, _>>().map_err(e)?
        };
        for id in &ids {
            self.delete_user(*id)?;
        }
        Ok(ids.len())
    }

    /// Bring the platform row of a game-server account up to date before
    /// use: its name and disabled flag, and, when the id now belongs to a
    /// different account (SQLite may reuse the id of a deleted last row;
    /// told apart by the creation time), drop what the old account left.
    pub fn sync_user(&mut self, user_id: i64, created: i64, name: &str, disabled: bool, now: i64) -> R<()> {
        let stored: Option<i64> =
            self.db.query_row("SELECT created FROM platform_users WHERE user_id = ?1", params![user_id], |r| r.get(0)).optional().map_err(e)?;
        if stored.is_some_and(|c| c != created) {
            self.delete_user(user_id)?;
        }
        self.db
            .execute(
                "INSERT INTO platform_users(user_id, display_name, disabled, created, updated) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(user_id) DO UPDATE SET display_name = excluded.display_name, disabled = excluded.disabled",
                params![user_id, name, i64::from(disabled), created, now],
            )
            .map(|_| ())
            .map_err(e)
    }

    /// A platform row for a new account (sign-up). Idempotent.
    pub fn ensure_user(&self, user_id: i64, display_name: Option<&str>, now: i64) -> R<()> {
        self.db
            .execute(
                "INSERT OR IGNORE INTO platform_users(user_id, display_name, created, updated) VALUES (?1, ?2, ?3, ?3)",
                params![user_id, display_name, now],
            )
            .map(|_| ())
            .map_err(e)
    }

    pub fn set_disabled(&self, user_id: i64, disabled: bool, now: i64) -> R<()> {
        self.ensure_user(user_id, None, now)?;
        self.db
            .execute("UPDATE platform_users SET disabled = ?1, updated = ?2 WHERE user_id = ?3", params![i64::from(disabled), now, user_id])
            .map(|_| ())
            .map_err(e)
    }

    pub fn set_roles(&self, user_id: i64, roles: &[&str], now: i64) -> R<()> {
        self.ensure_user(user_id, None, now)?;
        self.db
            .execute("UPDATE platform_users SET roles = ?1, updated = ?2 WHERE user_id = ?3", params![roles.join(","), now, user_id])
            .map(|_| ())
            .map_err(e)
    }

    /// The player said "not now" to adding an address.
    pub fn dismiss_email_prompt(&self, user_id: i64, now: i64) -> R<()> {
        self.ensure_user(user_id, None, now)?;
        self.db
            .execute("UPDATE platform_users SET email_prompt_dismissed = 1, updated = ?1 WHERE user_id = ?2", params![now, user_id])
            .map(|_| ())
            .map_err(e)
    }

    /// The account's platform view. `disabled_elsewhere`: the game
    /// server's own disabled flag (until the platform owns accounts).
    pub fn profile(&self, user_id: i64, disabled_elsewhere: bool, now: i64) -> R<Profile> {
        type Row = (Option<String>, Option<i64>, Option<String>, String, bool, bool);
        let row: Option<Row> = self
            .db
            .query_row(
                "SELECT email, email_verified_at, display_name, roles, disabled, email_prompt_dismissed FROM platform_users WHERE user_id = ?1",
                params![user_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? != 0, r.get::<_, i64>(5)? != 0)),
            )
            .optional()
            .map_err(e)?;
        let (email, verified_at, display_name, roles, disabled, dismissed) = row.unwrap_or_default();
        let pending_email = self.live(user_id, Purpose::VerifyEmail, now)?.first().map(|t| t.email.clone());
        let state = if disabled || disabled_elsewhere {
            AccountState::Disabled
        } else if email.is_some() {
            // A change of address waiting for its code keeps the old one.
            AccountState::Verified
        } else if pending_email.is_some() {
            AccountState::Unverified
        } else {
            AccountState::NoEmail
        };
        Ok(Profile {
            user_id,
            prompt_email: state == AccountState::NoEmail && !dismissed,
            state,
            email,
            pending_email,
            email_verified_at: verified_at,
            display_name,
            roles: roles.split(',').filter(|r| !r.is_empty()).map(str::to_string).collect(),
            supporter_flags: self.supporter_flags(user_id, now)?,
        })
    }

    /// The account whose verified address is `email`.
    pub fn user_for_email(&self, email: &str) -> R<Option<i64>> {
        self.db
            .query_row("SELECT user_id FROM platform_users WHERE email_key = ?1", params![email_key(email)], |r| r.get(0))
            .optional()
            .map_err(e)
    }

    // ---- codes ----------------------------------------------------------

    fn live(&self, user_id: i64, purpose: Purpose, now: i64) -> R<Vec<TokenRow>> {
        let mut st = self
            .db
            .prepare(
                "SELECT id, email, code_hash FROM platform_tokens
                 WHERE user_id = ?1 AND purpose = ?2 AND used IS NULL AND expires > ?3 AND tries < ?4 ORDER BY created DESC",
            )
            .map_err(e)?;
        let rows = st
            .query_map(params![user_id, purpose.as_str(), now, MAX_TRIES], |r| {
                Ok(TokenRow { id: r.get(0)?, email: r.get(1)?, code_hash: r.get(2)? })
            })
            .map_err(e)?;
        rows.collect::<Result<_, _>>().map_err(e)
    }

    fn rate_check(&self, user_id: i64, purpose: Purpose, now: i64) -> R<Result<(), Problem>> {
        let (count, last): (i64, Option<i64>) = self
            .db
            .query_row(
                "SELECT count(*), max(created) FROM platform_tokens WHERE user_id = ?1 AND purpose = ?2 AND created > ?3",
                params![user_id, purpose.as_str(), now - 24 * 3600],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(e)?;
        Ok(if last.is_some_and(|t| now - t < RESEND_SECONDS) {
            Err(Problem::TooSoon)
        } else if count >= EMAILS_PER_DAY {
            Err(Problem::TooManyEmails)
        } else {
            Ok(())
        })
    }

    fn issue(&self, user_id: i64, purpose: Purpose, email: &str, now: i64) -> R<Issued> {
        let id = platform_sdk::b64::encode(&crate::random_bytes::<16>());
        let code = codes::new_code();
        let expires = now + purpose.lifetime();
        // Only the newest code of a kind works.
        self.db
            .execute(
                "UPDATE platform_tokens SET used = ?1 WHERE user_id = ?2 AND purpose = ?3 AND used IS NULL",
                params![now, user_id, purpose.as_str()],
            )
            .map_err(e)?;
        self.db
            .execute(
                "INSERT INTO platform_tokens(id, purpose, user_id, email, code_hash, created, expires) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, purpose.as_str(), user_id, email, self.keys.code_hash(purpose.as_str(), user_id, &code), now, expires],
            )
            .map_err(e)?;
        Ok(Issued { purpose, user_id, email: email.to_string(), code: codes::shown(&code), link: self.keys.link(purpose.as_str(), &id, expires), expires })
    }

    /// The live code of `purpose` that matches `code`, counting a wrong try
    /// against every live code of that kind.
    fn match_code(&self, user_id: i64, purpose: Purpose, code: &str, now: i64) -> R<Result<TokenRow, Problem>> {
        let live = self.live(user_id, purpose, now)?;
        if live.is_empty() {
            let burnt: i64 = self
                .db
                .query_row(
                    "SELECT count(*) FROM platform_tokens WHERE user_id = ?1 AND purpose = ?2 AND used IS NULL AND expires > ?3 AND tries >= ?4",
                    params![user_id, purpose.as_str(), now, MAX_TRIES],
                    |r| r.get(0),
                )
                .map_err(e)?;
            return Ok(Err(if burnt > 0 { Problem::TooManyTries } else { Problem::NoCode }));
        }
        let hash = self.keys.code_hash(purpose.as_str(), user_id, code);
        if let Some(row) = live.into_iter().find(|t| t.code_hash == hash) {
            return Ok(Ok(row));
        }
        self.db
            .execute(
                "UPDATE platform_tokens SET tries = tries + 1 WHERE user_id = ?1 AND purpose = ?2 AND used IS NULL AND expires > ?3",
                params![user_id, purpose.as_str(), now],
            )
            .map_err(e)?;
        Ok(Err(if self.live(user_id, purpose, now)?.is_empty() { Problem::TooManyTries } else { Problem::BadCode }))
    }

    /// Ask to verify `email` for `user_id`: a code to send to that address.
    /// The account's verified address (if any) stays until the code is
    /// entered.
    pub fn start_email_verification(&self, user_id: i64, email: &str, now: i64) -> R<Result<Issued, Problem>> {
        let email = email.trim();
        if let Err(p) = check_email(email) {
            return Ok(Err(p));
        }
        match self.user_for_email(email)? {
            Some(owner) if owner == user_id => return Ok(Err(Problem::AlreadyVerified)),
            Some(_) => return Ok(Err(Problem::EmailTaken)),
            None => {}
        }
        if let Err(p) = self.rate_check(user_id, Purpose::VerifyEmail, now)? {
            return Ok(Err(p));
        }
        self.ensure_user(user_id, None, now)?;
        self.issue(user_id, Purpose::VerifyEmail, email, now).map(Ok)
    }

    fn finish_verification(&mut self, user_id: i64, row: &TokenRow, now: i64) -> R<Result<String, Problem>> {
        let tx = self.db.transaction().map_err(e)?;
        let owner: Option<i64> = tx
            .query_row("SELECT user_id FROM platform_users WHERE email_key = ?1", params![email_key(&row.email)], |r| r.get(0))
            .optional()
            .map_err(e)?;
        if owner.is_some_and(|o| o != user_id) {
            return Ok(Err(Problem::EmailTaken));
        }
        tx.execute(
            "INSERT OR IGNORE INTO platform_users(user_id, created, updated) VALUES (?1, ?2, ?2)",
            params![user_id, now],
        )
        .map_err(e)?;
        tx.execute(
            "UPDATE platform_users SET email = ?1, email_key = ?2, email_verified_at = ?3, updated = ?3 WHERE user_id = ?4",
            params![row.email, email_key(&row.email), now, user_id],
        )
        .map_err(e)?;
        tx.execute("UPDATE platform_tokens SET used = ?1 WHERE id = ?2", params![now, row.id]).map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(Ok(row.email.clone()))
    }

    /// Enter the code from the verification email. Returns the address now
    /// verified.
    pub fn verify_email(&mut self, user_id: i64, code: &str, now: i64) -> R<Result<String, Problem>> {
        match self.match_code(user_id, Purpose::VerifyEmail, code, now)? {
            Ok(row) => self.finish_verification(user_id, &row, now),
            Err(p) => Ok(Err(p)),
        }
    }

    /// Follow a signed verification link (the web page, later). Returns the
    /// account and the address now verified.
    pub fn verify_link(&mut self, link: &str, now: i64) -> R<Result<(i64, String), Problem>> {
        let Some((purpose, id)) = self.keys.open_link(link, now) else { return Ok(Err(Problem::NoCode)) };
        if Purpose::parse(&purpose) != Some(Purpose::VerifyEmail) {
            return Ok(Err(Problem::NoCode));
        }
        let row: Option<(i64, TokenRow)> = self
            .db
            .query_row(
                "SELECT user_id, id, email, code_hash FROM platform_tokens WHERE id = ?1 AND purpose = ?2 AND used IS NULL AND expires > ?3 AND tries < ?4",
                params![id, purpose, now, MAX_TRIES],
                |r| Ok((r.get(0)?, TokenRow { id: r.get(1)?, email: r.get(2)?, code_hash: r.get(3)? })),
            )
            .optional()
            .map_err(e)?;
        let Some((user_id, row)) = row else { return Ok(Err(Problem::NoCode)) };
        Ok(self.finish_verification(user_id, &row, now)?.map(|email| (user_id, email)))
    }

    /// Forget the account's address (verified and pending).
    pub fn remove_email(&self, user_id: i64, now: i64) -> R<()> {
        self.db
            .execute(
                "UPDATE platform_users SET email = NULL, email_key = NULL, email_verified_at = NULL, updated = ?1 WHERE user_id = ?2",
                params![now, user_id],
            )
            .map_err(e)?;
        self.db.execute("UPDATE platform_tokens SET used = ?1 WHERE user_id = ?2 AND used IS NULL", params![now, user_id]).map_err(e)?;
        Ok(())
    }

    /// Ask for a password-reset code, sent to the account's verified
    /// address.
    pub fn start_password_reset(&self, user_id: i64, now: i64) -> R<Result<Issued, Problem>> {
        let profile = self.profile(user_id, false, now)?;
        if profile.state == AccountState::Disabled {
            return Ok(Err(Problem::Disabled));
        }
        let Some(email) = profile.email else { return Ok(Err(Problem::NoVerifiedEmail)) };
        if let Err(p) = self.rate_check(user_id, Purpose::ResetPassword, now)? {
            return Ok(Err(p));
        }
        self.issue(user_id, Purpose::ResetPassword, &email, now).map(Ok)
    }

    /// Check a reset code before the new password is hashed. Returns the
    /// code's id for [`Self::finish_password_reset`]; a wrong code counts.
    pub fn check_password_reset(&self, user_id: i64, code: &str, now: i64) -> R<Result<String, Problem>> {
        Ok(self.match_code(user_id, Purpose::ResetPassword, code, now)?.map(|row| row.id))
    }

    /// The new password is stored: the code (and any other reset code of
    /// the account) stops working.
    pub fn finish_password_reset(&self, user_id: i64, now: i64) -> R<()> {
        self.db
            .execute(
                "UPDATE platform_tokens SET used = ?1 WHERE user_id = ?2 AND purpose = 'reset_password' AND used IS NULL",
                params![now, user_id],
            )
            .map(|_| ())
            .map_err(e)
    }

    /// Drop codes that ended more than a week ago (the server calls this
    /// now and then).
    pub fn prune(&self, now: i64) -> R<usize> {
        self.db
            .execute("DELETE FROM platform_tokens WHERE expires < ?1 OR used < ?1", params![now - 7 * 24 * 3600])
            .map_err(e)
    }

    // ---- supporters and linked accounts -----------------------------------

    /// Set (or renew) a supporter record from `source` ("kofi", "patreon",
    /// "manual"); `until` none: no end.
    #[allow(clippy::too_many_arguments)]
    pub fn set_supporter(&self, user_id: i64, source: &str, external_id: Option<&str>, tier: Option<&str>, flags: u32, until: Option<i64>, now: i64) -> R<()> {
        self.db
            .execute(
                "INSERT INTO platform_supporters(user_id, source, external_id, tier, flags, since, until) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(user_id, source) DO UPDATE SET external_id = excluded.external_id, tier = excluded.tier, flags = excluded.flags, until = excluded.until",
                params![user_id, source, external_id, tier, i64::from(flags), now, until],
            )
            .map(|_| ())
            .map_err(e)
    }

    pub fn end_supporter(&self, user_id: i64, source: &str) -> R<bool> {
        self.db
            .execute("DELETE FROM platform_supporters WHERE user_id = ?1 AND source = ?2", params![user_id, source])
            .map(|n| n > 0)
            .map_err(e)
    }

    /// OR of the flags of every supporter record still running at `now`.
    pub fn supporter_flags(&self, user_id: i64, now: i64) -> R<u32> {
        let mut st = self
            .db
            .prepare("SELECT flags FROM platform_supporters WHERE user_id = ?1 AND (until IS NULL OR until > ?2)")
            .map_err(e)?;
        let flags = st.query_map(params![user_id, now], |r| r.get::<_, i64>(0)).map_err(e)?;
        let all = flags.map(|f| f.map(|f| f as u32)).try_fold(0u32, |acc, f| f.map(|f| acc | f)).map_err(e);
        all
    }

    /// Link an external account (Ko-fi email hash, Patreon user id) to a
    /// platform account.
    pub fn link_account(&self, provider: &str, external_id: &str, user_id: i64, now: i64) -> R<()> {
        self.db
            .execute(
                "INSERT OR REPLACE INTO platform_links(provider, external_id, user_id, linked) VALUES (?1, ?2, ?3, ?4)",
                params![provider, external_id, user_id, now],
            )
            .map(|_| ())
            .map_err(e)
    }

    pub fn linked_user(&self, provider: &str, external_id: &str) -> R<Option<i64>> {
        self.db
            .query_row(
                "SELECT user_id FROM platform_links WHERE provider = ?1 AND external_id = ?2",
                params![provider, external_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)
    }

    /// Record a webhook delivery; false when it was seen before (providers
    /// retry, so handling must be idempotent).
    pub fn first_delivery(&self, provider: &str, event_id: &str, now: i64) -> R<bool> {
        self.db
            .execute(
                "INSERT OR IGNORE INTO platform_webhook_events(provider, event_id, received) VALUES (?1, ?2, ?3)",
                params![provider, event_id, now],
            )
            .map(|n| n > 0)
            .map_err(e)
    }

    // ---- data protection ------------------------------------------------

    /// Everything the platform tables hold about `user_id` (a subject
    /// access request), as JSON.
    pub fn export_user(&self, user_id: i64, now: i64) -> R<serde_json::Value> {
        let profile = self.profile(user_id, false, now)?;
        let mut st = self
            .db
            .prepare("SELECT source, tier, flags, since, until FROM platform_supporters WHERE user_id = ?1")
            .map_err(e)?;
        let supporters: Vec<serde_json::Value> = st
            .query_map(params![user_id], |r| {
                Ok(serde_json::json!({"source": r.get::<_, String>(0)?, "tier": r.get::<_, Option<String>>(1)?,
                    "flags": r.get::<_, i64>(2)?, "since": r.get::<_, i64>(3)?, "until": r.get::<_, Option<i64>>(4)?}))
            })
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        let mut st = self.db.prepare("SELECT provider, linked FROM platform_links WHERE user_id = ?1").map_err(e)?;
        let links: Vec<serde_json::Value> = st
            .query_map(params![user_id], |r| Ok(serde_json::json!({"provider": r.get::<_, String>(0)?, "linked": r.get::<_, i64>(1)?})))
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        Ok(serde_json::json!({"profile": profile, "supporters": supporters, "linked_accounts": links}))
    }

    /// Erase every platform row of `user_id` (account deletion). Returns
    /// the rows removed.
    pub fn delete_user(&mut self, user_id: i64) -> R<usize> {
        let tx = self.db.transaction().map_err(e)?;
        let mut n = 0;
        for table in ["platform_users", "platform_tokens", "platform_supporters", "platform_links"] {
            n += tx.execute(&format!("DELETE FROM {table} WHERE user_id = ?1"), params![user_id]).map_err(e)?;
        }
        tx.commit().map_err(e)?;
        Ok(n)
    }
}

struct TokenRow {
    id: String,
    email: String,
    code_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn store() -> PlatformStore {
        PlatformStore::in_memory(&[9; 32]).unwrap()
    }

    #[test]
    fn email_addresses_are_checked_plainly() {
        for ok in ["a@b.co", "yugi.muto+roses@example.com", "x@sub.example.org", "ünï@exämple.de"] {
            assert_eq!(check_email(ok), Ok(()), "{ok}");
        }
        for bad in ["", "a", "a@b", "@b.co", "a@@b.co", "a b@c.co", "a@b..co", ".a@b.co", "a@-b.co", "a@b.co\n"] {
            assert_eq!(check_email(bad), Err(Problem::BadEmail), "{bad:?}");
        }
    }

    #[test]
    fn verifying_an_address_end_to_end() {
        let mut s = store();
        assert_eq!(s.profile(1, false, NOW).unwrap().state, AccountState::NoEmail);
        assert!(s.profile(1, false, NOW).unwrap().prompt_email);
        let issued = s.start_email_verification(1, " Yugi@Example.com ", NOW).unwrap().unwrap();
        assert_eq!(issued.email, "Yugi@Example.com");
        let p = s.profile(1, false, NOW).unwrap();
        assert_eq!((p.state, p.pending_email.as_deref(), p.email.as_deref()), (AccountState::Unverified, Some("Yugi@Example.com"), None));
        assert_eq!(s.verify_email(1, "AAAA-AAAA", NOW), Ok(Err(Problem::BadCode)));
        assert_eq!(s.verify_email(1, &issued.code.to_lowercase(), NOW + 5).unwrap().unwrap(), "Yugi@Example.com");
        let p = s.profile(1, false, NOW).unwrap();
        assert_eq!((p.state, p.email.as_deref(), p.email_verified_at), (AccountState::Verified, Some("Yugi@Example.com"), Some(NOW + 5)));
        assert!(!p.prompt_email);
        // Used: the code works once.
        assert_eq!(s.verify_email(1, &issued.code, NOW + 6), Ok(Err(Problem::NoCode)));
        assert_eq!(s.user_for_email("yugi@example.COM").unwrap(), Some(1));
        assert_eq!(s.start_email_verification(1, "yugi@example.com", NOW + 100), Ok(Err(Problem::AlreadyVerified)));
        assert_eq!(s.start_email_verification(2, "YUGI@example.com", NOW + 100), Ok(Err(Problem::EmailTaken)));
        assert_eq!(s.profile(1, true, NOW).unwrap().state, AccountState::Disabled);
    }

    #[test]
    fn codes_expire_resend_is_limited_and_tries_run_out() {
        let mut s = store();
        let first = s.start_email_verification(1, "a@b.co", NOW).unwrap().unwrap();
        assert_eq!(s.start_email_verification(1, "a@b.co", NOW + 10), Ok(Err(Problem::TooSoon)));
        let second = s.start_email_verification(1, "a@b.co", NOW + RESEND_SECONDS).unwrap().unwrap();
        // Only the newest code works.
        assert_eq!(s.verify_email(1, &first.code, NOW + 70), Ok(Err(Problem::BadCode)));
        for _ in 1..MAX_TRIES - 1 {
            assert_eq!(s.verify_email(1, "ZZZZ-ZZZZ", NOW + 70), Ok(Err(Problem::BadCode)));
        }
        assert_eq!(s.verify_email(1, "ZZZZ-ZZZZ", NOW + 70), Ok(Err(Problem::TooManyTries)));
        assert_eq!(s.verify_email(1, &second.code, NOW + 71), Ok(Err(Problem::TooManyTries)), "burnt even with the right code");
        let mut t = NOW + 200;
        for _ in 2..EMAILS_PER_DAY {
            s.start_email_verification(1, "a@b.co", t).unwrap().unwrap();
            t += RESEND_SECONDS;
        }
        assert_eq!(s.start_email_verification(1, "a@b.co", t), Ok(Err(Problem::TooManyEmails)));
        // A day later: allowed again, and an old code has expired.
        let late = s.start_email_verification(1, "a@b.co", NOW + 25 * 3600).unwrap().unwrap();
        assert_eq!(s.verify_email(1, &late.code, late.expires + 1), Ok(Err(Problem::NoCode)));
    }

    #[test]
    fn a_signed_link_verifies_like_the_code() {
        let mut s = store();
        let issued = s.start_email_verification(7, "link@example.com", NOW).unwrap().unwrap();
        assert_eq!(s.verify_link(&issued.link, NOW + 1).unwrap(), Ok((7, "link@example.com".to_string())));
        assert_eq!(s.verify_link(&issued.link, NOW + 2).unwrap(), Err(Problem::NoCode));
        assert_eq!(s.verify_link("v1.verify_email.x.9999999999.AAAA", NOW).unwrap(), Err(Problem::NoCode));
    }

    #[test]
    fn password_reset_needs_a_verified_address() {
        let mut s = store();
        assert_eq!(s.start_password_reset(3, NOW), Ok(Err(Problem::NoVerifiedEmail)));
        let v = s.start_email_verification(3, "r@example.com", NOW).unwrap().unwrap();
        s.verify_email(3, &v.code, NOW).unwrap().unwrap();
        let reset = s.start_password_reset(3, NOW + 10).unwrap().unwrap();
        assert_eq!(reset.email, "r@example.com");
        assert_eq!(reset.expires, NOW + 10 + RESET_SECONDS);
        assert_eq!(s.check_password_reset(3, "WRONG-CODE", NOW + 20), Ok(Err(Problem::BadCode)));
        assert!(s.check_password_reset(3, &reset.code, NOW + 20).unwrap().is_ok());
        s.finish_password_reset(3, NOW + 21).unwrap();
        assert_eq!(s.check_password_reset(3, &reset.code, NOW + 22), Ok(Err(Problem::NoCode)));
        // A verification code is not a reset code.
        let again = s.start_password_reset(3, NOW + 200).unwrap().unwrap();
        assert_eq!(s.check_password_reset(3, &again.code, again.expires), Ok(Err(Problem::NoCode)));
        s.set_disabled(3, true, NOW).unwrap();
        assert_eq!(s.start_password_reset(3, NOW + 400), Ok(Err(Problem::Disabled)));
    }

    #[test]
    fn legacy_users_are_migrated_with_no_email() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL, name_key TEXT NOT NULL UNIQUE, phc TEXT NOT NULL,
                created INTEGER NOT NULL, disabled INTEGER NOT NULL DEFAULT 0, invite TEXT);
             INSERT INTO users(id, name, name_key, phc, created, disabled) VALUES (1, 'Yugi', 'yugi', 'x', 100, 0), (2, 'Kaiba', 'kaiba', 'x', 200, 1);",
        )
        .unwrap();
        let s = PlatformStore::from_connection(db, &[1; 32]).unwrap();
        let yugi = s.profile(1, false, NOW).unwrap();
        assert_eq!((yugi.state, yugi.email, yugi.display_name.as_deref(), yugi.prompt_email), (AccountState::NoEmail, None, Some("Yugi"), true));
        assert_eq!(s.profile(2, false, NOW).unwrap().state, AccountState::Disabled);
        // Opening again adds nothing twice.
        assert_eq!(s.migrate_legacy_users().unwrap(), 0);
        s.dismiss_email_prompt(1, NOW).unwrap();
        assert!(!s.profile(1, false, NOW).unwrap().prompt_email);
    }

    #[test]
    fn deleted_and_reused_legacy_ids_lose_the_old_address() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL, phc TEXT NOT NULL, created INTEGER NOT NULL, disabled INTEGER NOT NULL DEFAULT 0);
             INSERT INTO users(id, name, phc, created) VALUES (1, 'Yugi', 'x', 100), (2, 'Joey', 'x', 200);",
        )
        .unwrap();
        let mut s = PlatformStore::from_connection(db, &[1; 32]).unwrap();
        for (id, mail) in [(1, "y@example.com"), (2, "j@example.com")] {
            let i = s.start_email_verification(id, mail, NOW).unwrap().unwrap();
            s.verify_email(id, &i.code, NOW).unwrap().unwrap();
        }
        // Joey's account is deleted in the users table: his row goes.
        s.db.execute("DELETE FROM users WHERE id = 2", []).unwrap();
        assert_eq!(s.prune_legacy_orphans().unwrap(), 1);
        assert_eq!(s.user_for_email("j@example.com").unwrap(), None);
        // A new account that got Yugi's old id (another creation time) does
        // not inherit his address.
        s.sync_user(1, 100, "Yugi", false, NOW).unwrap();
        assert_eq!(s.user_for_email("y@example.com").unwrap(), Some(1));
        s.sync_user(1, 999, "Mai", false, NOW).unwrap();
        assert_eq!(s.user_for_email("y@example.com").unwrap(), None);
        let p = s.profile(1, false, NOW).unwrap();
        assert_eq!((p.state, p.display_name.as_deref()), (AccountState::NoEmail, Some("Mai")));
        s.sync_user(1, 999, "Mai", true, NOW).unwrap();
        assert_eq!(s.profile(1, false, NOW).unwrap().state, AccountState::Disabled);
    }

    #[test]
    fn a_file_database_reopens_and_refuses_a_newer_schema() {
        let dir = std::env::temp_dir().join(format!("platform-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("accounts.db");
        {
            let mut s = PlatformStore::open(&path, &[3; 32]).unwrap();
            let i = s.start_email_verification(1, "f@example.com", NOW).unwrap().unwrap();
            s.verify_email(1, &i.code, NOW).unwrap().unwrap();
        }
        let s = PlatformStore::open(&path, &[3; 32]).unwrap();
        assert_eq!(s.user_for_email("f@example.com").unwrap(), Some(1));
        s.db.execute("UPDATE platform_meta SET value = '99' WHERE key = 'schema'", []).unwrap();
        drop(s);
        assert!(PlatformStore::open(&path, &[3; 32]).err().unwrap().contains("newer"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn supporters_links_export_and_erasure() {
        let mut s = store();
        s.set_supporter(5, "kofi", Some("k-1"), Some("Coffee"), perks::BADGE, Some(NOW + 100), NOW).unwrap();
        s.set_supporter(5, "manual", None, None, perks::EARLY_LAUNCHER, None, NOW).unwrap();
        assert_eq!(s.supporter_flags(5, NOW).unwrap(), perks::BADGE | perks::EARLY_LAUNCHER);
        assert_eq!(s.supporter_flags(5, NOW + 200).unwrap(), perks::EARLY_LAUNCHER, "expired records stop counting");
        assert_eq!(s.profile(5, false, NOW).unwrap().supporter_flags, perks::BADGE | perks::EARLY_LAUNCHER);
        s.link_account("patreon", "p-77", 5, NOW).unwrap();
        assert_eq!(s.linked_user("patreon", "p-77").unwrap(), Some(5));
        assert!(s.first_delivery("kofi", "tx-1", NOW).unwrap());
        assert!(!s.first_delivery("kofi", "tx-1", NOW).unwrap());
        let i = s.start_email_verification(5, "s@example.com", NOW).unwrap().unwrap();
        s.verify_email(5, &i.code, NOW).unwrap().unwrap();
        let export = s.export_user(5, NOW).unwrap();
        assert_eq!(export["profile"]["email"], "s@example.com");
        assert_eq!(export["supporters"].as_array().unwrap().len(), 2);
        assert!(s.delete_user(5).unwrap() >= 4);
        assert_eq!(s.profile(5, false, NOW).unwrap().state, AccountState::NoEmail);
        assert_eq!(s.user_for_email("s@example.com").unwrap(), None);
        assert_eq!(s.linked_user("patreon", "p-77").unwrap(), None);
    }
}

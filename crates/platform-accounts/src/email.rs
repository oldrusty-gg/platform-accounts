//! Sending mail: the [`EmailSender`] trait, a development sender that
//! writes each message to a local file ([`LogSender`]), a provider HTTP
//! sender whose requests are built but not sent until a provider is chosen
//! ([`HttpApiSender`]), the message texts, and a background [`Mailer`] so
//! a slow provider never stalls a server loop.

use crate::store::{Issued, Purpose};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Email {
    pub to: String,
    pub subject: String,
    pub text: String,
    /// What kind of message (provider tag / stream; never the address).
    pub tag: String,
}

pub trait EmailSender: Send + Sync {
    fn send(&self, email: &Email) -> Result<(), String>;
    /// A short description for the server log ("log file D:\mail.jsonl").
    fn describe(&self) -> String;
}

/// Writes every message as one JSON line to a file instead of sending it
/// (development, tests, a private server whose owner reads the codes out).
pub struct LogSender {
    path: PathBuf,
    lock: Mutex<()>,
}

impl LogSender {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }

    /// The messages in a log file, oldest first.
    pub fn read(path: &Path) -> Vec<Email> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

impl EmailSender for LogSender {
    fn send(&self, email: &Email) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|_| "mail log lock poisoned".to_string())?;
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        let line = serde_json::to_string(email).map_err(|e| e.to_string())?;
        writeln!(file, "{line}").map_err(|e| format!("{}: {e}", self.path.display()))
    }

    fn describe(&self) -> String {
        format!("mail log file {}", self.path.display())
    }
}

/// A transactional email provider's HTTP API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// POST https://api.postmarkapp.com/email (server token header).
    Postmark,
    /// POST https://api.resend.com/emails (bearer token).
    Resend,
}

impl Provider {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "postmark" => Some(Self::Postmark),
            "resend" => Some(Self::Resend),
            _ => None,
        }
    }
}

/// An HTTP request ready for any client (no client is linked here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: &'static str,
    /// The API key appears only in these headers; never log them.
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

/// Sends through a provider's HTTP API. The request is built and checked
/// here; the network call is switched off until Jim chooses the provider
/// (then an HTTPS client, for example `ureq` with rustls, posts
/// [`HttpApiSender::request`] and checks the status).
pub struct HttpApiSender {
    pub provider: Provider,
    api_key: String,
    /// `"Brand <no-reply@mail.brand.example>"` on the verified domain.
    pub from: String,
}

impl HttpApiSender {
    pub fn new(provider: Provider, api_key: String, from: String) -> Self {
        Self { provider, api_key, from }
    }

    pub fn request(&self, email: &Email) -> HttpRequest {
        let json = |v: serde_json::Value| v.to_string();
        match self.provider {
            Provider::Postmark => HttpRequest {
                method: "POST",
                url: "https://api.postmarkapp.com/email",
                headers: vec![
                    ("Accept", "application/json".into()),
                    ("Content-Type", "application/json".into()),
                    ("X-Postmark-Server-Token", self.api_key.clone()),
                ],
                body: json(serde_json::json!({
                    "From": self.from, "To": email.to, "Subject": email.subject,
                    "TextBody": email.text, "Tag": email.tag, "MessageStream": "outbound",
                })),
            },
            Provider::Resend => HttpRequest {
                method: "POST",
                url: "https://api.resend.com/emails",
                headers: vec![("Content-Type", "application/json".into()), ("Authorization", format!("Bearer {}", self.api_key))],
                body: json(serde_json::json!({
                    "from": self.from, "to": [email.to], "subject": email.subject,
                    "text": email.text, "tags": [{"name": "kind", "value": email.tag}],
                })),
            },
        }
    }
}

impl EmailSender for HttpApiSender {
    fn send(&self, email: &Email) -> Result<(), String> {
        let _ = self.request(email);
        Err(format!("sending through {:?} is not switched on in this build (no HTTP client yet)", self.provider))
    }

    fn describe(&self) -> String {
        format!("{:?} API (not switched on)", self.provider)
    }
}

/// The message for a code just issued. `brand`: the platform's name;
/// `web`: the base URL of a page that takes the signed link (none yet:
/// the code is typed into the launcher).
pub fn message_for(issued: &Issued, brand: &str, account_name: &str, web: Option<&str>) -> Email {
    let minutes = |s: i64| s / 60;
    let link = web.map(|base| format!("\nOr open this link:\n{base}?t={}\n", issued.link)).unwrap_or_default();
    let (subject, text) = match issued.purpose {
        Purpose::VerifyEmail => (
            format!("{brand}: your code is {}", issued.code),
            format!(
                "Hello {account_name},\n\nType this code in the launcher's Profile page to confirm this email address for your {brand} account:\n\n    {}\n{link}\nThe code works for {} hours. If you did not ask for it, ignore this message: nothing changes.\n",
                issued.code,
                minutes(crate::store::VERIFY_SECONDS) / 60,
            ),
        ),
        Purpose::ResetPassword => (
            format!("{brand}: reset your password"),
            format!(
                "Hello {account_name},\n\nSomeone (hopefully you) asked to reset the password of your {brand} account. Type this code in the launcher:\n\n    {}\n{link}\nThe code works for {} minutes and once. If you did not ask, ignore this message: your password stays as it is.\n",
                issued.code,
                minutes(crate::store::RESET_SECONDS),
            ),
        ),
    };
    Email { to: issued.email.clone(), subject, text, tag: issued.purpose.as_str().into() }
}

/// Sends on its own thread. [`Mailer::queue`] never blocks; outcomes come
/// back as log lines (no addresses in them) from [`Mailer::notes`].
pub struct Mailer {
    queue: mpsc::SyncSender<Email>,
    notes: mpsc::Receiver<String>,
    describe: String,
}

/// Messages waiting at most; more are refused (the caller says "busy").
pub const MAILER_QUEUE: usize = 256;

impl Mailer {
    pub fn start(sender: Box<dyn EmailSender>) -> Self {
        let (queue, rx) = mpsc::sync_channel::<Email>(MAILER_QUEUE);
        let (note_tx, notes) = mpsc::channel();
        let describe = sender.describe();
        std::thread::spawn(move || {
            for email in rx {
                let note = match sender.send(&email) {
                    Ok(()) => format!("sent a {} email", email.tag),
                    Err(e) => format!("sending a {} email failed: {e}", email.tag),
                };
                if note_tx.send(note).is_err() {
                    return;
                }
            }
        });
        Self { queue, notes, describe }
    }

    pub fn describe(&self) -> &str {
        &self.describe
    }

    /// Queue `email`; false when the queue is full.
    pub fn queue(&self, email: Email) -> bool {
        self.queue.try_send(email).is_ok()
    }

    pub fn notes(&self) -> Vec<String> {
        self.notes.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issued(purpose: Purpose) -> Issued {
        Issued { purpose, user_id: 1, email: "yugi@example.com".into(), code: "ABCD-EFGH".into(), link: "v1.x.y.1.z".into(), expires: 0 }
    }

    #[test]
    fn messages_carry_the_code_and_the_brand() {
        let m = message_for(&issued(Purpose::VerifyEmail), "Platform", "Yugi", None);
        assert_eq!(m.to, "yugi@example.com");
        assert!(m.subject.contains("ABCD-EFGH") && m.subject.starts_with("Platform"));
        assert!(m.text.contains("Hello Yugi") && m.text.contains("24 hours") && !m.text.contains("http"));
        let r = message_for(&issued(Purpose::ResetPassword), "Platform", "Yugi", Some("https://example.org/reset"));
        assert!(r.text.contains("30 minutes") && r.text.contains("https://example.org/reset?t=v1.x.y.1.z"));
        assert_eq!(r.tag, "reset_password");
    }

    #[test]
    fn the_log_sender_writes_readable_lines() {
        let path = std::env::temp_dir().join(format!("platform-mail-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let sender = LogSender::new(&path);
        let m = message_for(&issued(Purpose::VerifyEmail), "P", "Yugi", None);
        sender.send(&m).unwrap();
        sender.send(&m).unwrap();
        assert_eq!(LogSender::read(&path), vec![m.clone(), m]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn provider_requests_are_built_but_not_sent() {
        let m = message_for(&issued(Purpose::VerifyEmail), "P", "Yugi", None);
        let pm = HttpApiSender::new(Provider::Postmark, "server-token".into(), "P <no-reply@mail.example.org>".into());
        let req = pm.request(&m);
        assert_eq!(req.url, "https://api.postmarkapp.com/email");
        assert!(req.headers.contains(&("X-Postmark-Server-Token", "server-token".into())));
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!((body["To"].as_str(), body["MessageStream"].as_str()), (Some("yugi@example.com"), Some("outbound")));
        let rs = HttpApiSender::new(Provider::Resend, "re_key".into(), "P <no-reply@mail.example.org>".into());
        let body: serde_json::Value = serde_json::from_str(&rs.request(&m).body).unwrap();
        assert_eq!(body["to"][0], "yugi@example.com");
        assert!(rs.send(&m).unwrap_err().contains("not switched on"));
        assert_eq!(Provider::parse("Postmark"), Some(Provider::Postmark));
    }

    #[test]
    fn the_mailer_sends_in_the_background() {
        let path = std::env::temp_dir().join(format!("platform-mailer-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mailer = Mailer::start(Box::new(LogSender::new(&path)));
        assert!(mailer.queue(message_for(&issued(Purpose::VerifyEmail), "P", "Yugi", None)));
        let start = std::time::Instant::now();
        let mut notes = Vec::new();
        while notes.is_empty() && start.elapsed().as_secs() < 5 {
            notes = mailer.notes();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(notes, vec!["sent a verify_email email".to_string()]);
        assert_eq!(LogSender::read(&path).len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}

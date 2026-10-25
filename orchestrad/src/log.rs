//! Log output for the service: one line to stderr, with an sd-daemon priority prefix when
//! the journal is listening.
//!
//! Callers pass names and ids only. Nothing here formats a config, an offer or a reply.

use std::sync::atomic::{AtomicBool, Ordering};

use orchestra_core::link::LinkLog;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
    Notice,
    Info,
}

impl Level {
    fn code(self) -> u8 {
        match self {
            Level::Error => 3,
            Level::Warning => 4,
            Level::Notice => 5,
            Level::Info => 6,
        }
    }
}

static PREFIX: AtomicBool = AtomicBool::new(false);

/// Turn the `<N>` prefixes on when `JOURNAL_STREAM` is set, which systemd does for a unit
/// whose stderr goes to the journal.
pub fn init(journal_stream: Option<&str>) {
    PREFIX.store(journal_stream.is_some_and(|v| !v.is_empty()), Ordering::SeqCst);
}

/// Control characters (newlines included) become spaces, so one message is one line and
/// text from a phone or a file cannot forge a priority prefix or a terminal escape.
pub fn clean(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// The line as written, without the newline.
pub fn line(level: Level, prefix: bool, message: &str) -> String {
    let message = clean(message);
    if prefix { format!("<{}>{message}", level.code()) } else { message }
}

pub fn log(level: Level, message: &str) {
    eprintln!("{}", line(level, PREFIX.load(Ordering::SeqCst), message));
}

pub fn error(message: &str) {
    log(Level::Error, message);
}

pub fn warn(message: &str) {
    log(Level::Warning, message);
}

pub fn notice(message: &str) {
    log(Level::Notice, message);
}

pub fn info(message: &str) {
    log(Level::Info, message);
}

/// What the host's link says, as a level and a sentence. Client and pairing ids, never a
/// key; the address of a refused peer is left out.
pub fn describe(event: &LinkLog) -> (Level, String) {
    match event {
        LinkLog::Listening { addr } => (Level::Notice, format!("link listening on {addr}")),
        LinkLog::Refused { .. } => {
            (Level::Warning, "link refused a connection (too many open, or too many attempts)".to_string())
        }
        LinkLog::PairingRequested { id } => (Level::Notice, format!("pairing requested ({id})")),
        LinkLog::Paired { client_id } => (Level::Notice, format!("client {client_id} paired")),
        LinkLog::Forbidden { client_id, method } => {
            (Level::Warning, format!("client {client_id} was refused {method}: no scope"))
        }
        LinkLog::Revoked { client_id } => (Level::Notice, format!("client {client_id} revoked")),
        LinkLog::Lagging { client_id } => {
            (Level::Warning, format!("client {client_id} fell behind and was disconnected"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_only_when_asked() {
        assert_eq!(line(Level::Error, true, "bad"), "<3>bad");
        assert_eq!(line(Level::Warning, true, "w"), "<4>w");
        assert_eq!(line(Level::Notice, true, "n"), "<5>n");
        assert_eq!(line(Level::Info, true, "i"), "<6>i");
        assert_eq!(line(Level::Info, false, "i"), "i");
    }

    #[test]
    fn one_message_is_one_line() {
        assert_eq!(line(Level::Info, true, "a\n<3>forged\r\x1b[2J"), "<6>a <3>forged  [2J");
        assert!(!clean("x\ny").contains('\n'));
    }

    #[test]
    fn link_events_name_ids_only() {
        let (level, text) = describe(&LinkLog::Paired { client_id: "c1".into() });
        assert_eq!((level, text.as_str()), (Level::Notice, "client c1 paired"));
        let (_, text) = describe(&LinkLog::Refused { ip: "10.1.2.3".parse().unwrap() });
        assert!(!text.contains("10.1.2.3"));
        let (level, text) = describe(&LinkLog::Listening { addr: "127.0.0.1:5000".parse().unwrap() });
        assert_eq!(level, Level::Notice);
        assert!(text.contains("127.0.0.1:5000"));
    }
}

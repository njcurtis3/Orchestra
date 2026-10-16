//! When to tell the user about a session. `decide` is pure: it sees the status before and
//! after one change and the kind of any inbox item that change added. The OS call itself is
//! made by the callback `lib.rs` hands to `Sessions`.
//!
//! The text carries the project name and the kind only. Prompts, questions and results can
//! hold anything from the user's project and stay out of a notification.

use crate::session::state::{InboxKind, SessionStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// A permission request, a question, or a request Orchestra cannot answer.
    NeedsInput,
    /// A turn ended cleanly.
    Finished,
    /// A turn ended with an error.
    Errored,
    /// The session process died.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
}

fn is_waiting(s: SessionStatus) -> bool {
    matches!(s, SessionStatus::WaitingPermission | SessionStatus::WaitingQuestion | SessionStatus::NeedsAttention)
}

/// `new_item` is the kind of the inbox item the change added, if it added one.
pub fn decide(before: SessionStatus, after: SessionStatus, new_item: Option<InboxKind>) -> Option<Notice> {
    let kind = match after {
        s if is_waiting(s) && !is_waiting(before) => NoticeKind::NeedsInput,
        SessionStatus::Idle if before != SessionStatus::Idle => match new_item {
            Some(InboxKind::Finished) => NoticeKind::Finished,
            Some(InboxKind::Error) => NoticeKind::Errored,
            _ => return None,
        },
        SessionStatus::Failed if before != SessionStatus::Failed => NoticeKind::Failed,
        _ => return None,
    };
    Some(Notice { kind })
}

/// The notification body.
pub fn body(project_name: &str, kind: NoticeKind) -> String {
    let what = match kind {
        NoticeKind::NeedsInput => "needs your input",
        NoticeKind::Finished => "finished",
        NoticeKind::Errored => "turn failed",
        NoticeKind::Failed => "session failed",
    };
    format!("{project_name}: {what}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use SessionStatus::*;

    fn kind(before: SessionStatus, after: SessionStatus, item: Option<InboxKind>) -> Option<NoticeKind> {
        decide(before, after, item).map(|n| n.kind)
    }

    #[test]
    fn needs_input_on_entering_wait() {
        for after in [WaitingPermission, WaitingQuestion, NeedsAttention] {
            for before in [Working, Starting, Idle] {
                assert_eq!(kind(before, after, Some(InboxKind::Permission)), Some(NoticeKind::NeedsInput), "{before:?} -> {after:?}");
            }
        }
    }

    #[test]
    fn no_notice_wait_to_wait() {
        for before in [WaitingPermission, WaitingQuestion, NeedsAttention] {
            for after in [WaitingPermission, WaitingQuestion, NeedsAttention] {
                assert_eq!(kind(before, after, Some(InboxKind::Question)), None, "{before:?} -> {after:?}");
            }
        }
    }

    #[test]
    fn finished_on_turn_end() {
        assert_eq!(kind(Working, Idle, Some(InboxKind::Finished)), Some(NoticeKind::Finished));
        assert_eq!(kind(NeedsAttention, Idle, Some(InboxKind::Finished)), Some(NoticeKind::Finished));
        assert_eq!(kind(Working, Idle, None), None, "no item, no notice");
        assert_eq!(kind(Idle, Idle, Some(InboxKind::Finished)), None, "not entering idle");
    }

    #[test]
    fn errored_on_error_result() {
        assert_eq!(kind(Working, Idle, Some(InboxKind::Error)), Some(NoticeKind::Errored));
        assert_eq!(kind(Working, Failed, Some(InboxKind::Error)), Some(NoticeKind::Failed), "a dead process is Failed, not a failed turn");
        assert_eq!(kind(Failed, Failed, Some(InboxKind::Error)), None);
    }

    #[test]
    fn no_notice_on_stop_or_suspend() {
        for before in [Starting, Working, WaitingPermission, WaitingQuestion, NeedsAttention, Idle, Suspended] {
            assert_eq!(kind(before, Ended, Some(InboxKind::Error)), None, "{before:?} -> ended");
            assert_eq!(kind(before, Suspended, None), None, "{before:?} -> suspended");
        }
        assert_eq!(kind(Starting, Working, None), None);
        assert_eq!(kind(Suspended, Working, None), None, "a resume is not news");
    }

    #[test]
    fn body_names_project_and_kind_only() {
        assert_eq!(body("proj", NoticeKind::NeedsInput), "proj: needs your input");
        assert_eq!(body("proj", NoticeKind::Finished), "proj: finished");
        assert_eq!(body("proj", NoticeKind::Errored), "proj: turn failed");
        assert_eq!(body("proj", NoticeKind::Failed), "proj: session failed");
    }
}

//! A process group per session: the Unix counterpart of the Windows job in `job.rs`.
//! The child leads a new group (`process_group(0)`), so its pgid is its pid and there is no
//! window between spawn and assignment. `terminate` signals the whole group, `active`
//! reports whether any member is left. A grandchild that calls `setsid` leaves the group,
//! as a breakaway child leaves a job; that is not handled.

use std::sync::atomic::{AtomicBool, Ordering};

pub struct Group {
    pgid: libc::pid_t,
    /// Latched once the group has emptied. A pgid can be recycled after that, so nothing is
    /// signalled again: a `Drop` hours later must not hit an unrelated process.
    gone: AtomicBool,
}

impl Group {
    /// `leader` is the pid of a child spawned with `process_group(0)`.
    pub fn new(leader: u32) -> Option<Group> {
        let pgid = libc::pid_t::try_from(leader).ok().filter(|p| *p > 1)?;
        Some(Group { pgid, gone: AtomicBool::new(false) })
    }

    /// SIGKILL every process in the group. A no-op once the group has emptied.
    pub fn terminate(&self) {
        if self.gone.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: kill takes plain integers; pgid > 1 so this is never "every process".
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
    }

    /// 1 while the group has a member (a zombie counts until it is reaped), 0 once empty.
    pub fn active(&self) -> u32 {
        if self.gone.load(Ordering::SeqCst) {
            return 0;
        }
        // SAFETY: signal 0 only checks that the group exists.
        let alive = unsafe { libc::kill(-self.pgid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if alive {
            1
        } else {
            self.gone.store(true, Ordering::SeqCst);
            0
        }
    }

    #[cfg(test)]
    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }
}

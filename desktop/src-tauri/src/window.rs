//! Keeps the restored main window reachable. The window-state plugin restores the saved
//! size and position, and only restores the position when the saved rect intersects a
//! monitor; it restores the size unclamped and never checks the title bar is reachable.
//! This is the check that does: the title-bar strip must be grabbable on some monitor,
//! else the window is clamped to the primary monitor and centred.

use tauri::{Runtime, WebviewWindow};

/// How much of the title bar must lie inside a monitor for the window to be reachable.
const MIN_GRAB_W: i64 = 100;
const STRIP_H: i64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn right(&self) -> i64 {
        i64::from(self.x) + i64::from(self.w)
    }

    fn bottom(&self) -> i64 {
        i64::from(self.y) + i64::from(self.h)
    }
}

/// True when at least `MIN_GRAB_W` x `STRIP_H` px of the window's title-bar strip (its top
/// `STRIP_H` px, or the whole window if shorter) lies inside one monitor work area.
pub fn is_visible(window: Rect, monitors: &[Rect]) -> bool {
    let strip_h = i64::from(window.h).min(STRIP_H);
    let need_w = i64::from(window.w).min(MIN_GRAB_W);
    let strip_bottom = i64::from(window.y) + strip_h;
    monitors.iter().any(|m| {
        let w = window.right().min(m.right()) - i64::from(window.x).max(i64::from(m.x));
        let h = strip_bottom.min(m.bottom()) - i64::from(window.y).max(i64::from(m.y));
        w >= need_w && h >= strip_h
    })
}

/// `None` when the window is fine where it is. Otherwise a rect on the primary monitor:
/// the size is clamped to its work area and the window is centred in it.
pub fn recentred(window: Rect, monitors: &[Rect], primary: Rect) -> Option<Rect> {
    if is_visible(window, monitors) {
        return None;
    }
    let w = window.w.min(primary.w);
    let h = window.h.min(primary.h);
    Some(Rect {
        x: primary.x + ((primary.w - w) / 2) as i32,
        y: primary.y + ((primary.h - h) / 2) as i32,
        w,
        h,
    })
}

/// Run once after the plugin has restored the window. Does nothing if any display
/// information is unavailable, so a failure here can never hide the window.
pub fn ensure_visible<R: Runtime>(window: &WebviewWindow<R>) {
    if window.is_maximized().unwrap_or(false) || window.is_fullscreen().unwrap_or(false) {
        return;
    }
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return;
    };
    let work_area = |m: &tauri::Monitor| {
        let a = m.work_area();
        Rect { x: a.position.x, y: a.position.y, w: a.size.width, h: a.size.height }
    };
    let Ok(monitors) = window.available_monitors() else {
        return;
    };
    let monitors: Vec<Rect> = monitors.iter().map(work_area).collect();
    let primary = window.primary_monitor().ok().flatten().map(|m| work_area(&m)).or_else(|| monitors.first().copied());
    let Some(primary) = primary else {
        return;
    };
    let current = Rect { x: pos.x, y: pos.y, w: size.width, h: size.height };
    if let Some(fit) = recentred(current, &monitors, primary) {
        let _ = window.set_size(tauri::PhysicalSize::new(fit.w, fit.h));
        let _ = window.set_position(tauri::PhysicalPosition::new(fit.x, fit.y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: Rect = Rect { x: 0, y: 0, w: 1920, h: 1040 };

    fn win(x: i32, y: i32) -> Rect {
        Rect { x, y, w: 1100, h: 720 }
    }

    #[test]
    fn offscreen_rect_is_not_visible() {
        assert!(!is_visible(win(-5000, -5000), &[PRIMARY]));
        assert!(!is_visible(win(4000, 100), &[PRIMARY]), "right of every monitor");
        assert!(!is_visible(win(100, -40), &[PRIMARY]), "title bar above the top edge");
        assert!(!is_visible(win(100, 1030), &[PRIMARY]), "title bar below the bottom edge");
        assert!(!is_visible(win(100, 100), &[]), "no monitors known");
    }

    #[test]
    fn partially_visible_title_bar_is_visible() {
        assert!(is_visible(win(100, 100), &[PRIMARY]));
        assert!(is_visible(win(1820, 100), &[PRIMARY]), "exactly 100 px of the strip inside");
        assert!(!is_visible(win(1821, 100), &[PRIMARY]), "99 px is not enough");
        assert!(is_visible(win(-1000, 0), &[PRIMARY]), "100 px at the left edge");
        let second = Rect { x: 1920, y: 0, w: 1920, h: 1040 };
        assert!(is_visible(win(1900, 100), &[PRIMARY, second]), "straddling two monitors counts");
        assert!(is_visible(win(2500, 100), &[PRIMARY, second]));
    }

    #[test]
    fn rect_on_removed_monitor_is_recentred() {
        // Saved on a second monitor that is no longer attached.
        let fit = recentred(win(2500, 100), &[PRIMARY], PRIMARY).unwrap();
        assert_eq!(fit, Rect { x: 410, y: 160, w: 1100, h: 720 });
        assert!(is_visible(fit, &[PRIMARY]));
        // Bigger than the primary work area: clamped, then centred.
        let huge = Rect { x: 9000, y: 9000, w: 5000, h: 3000 };
        let fit = recentred(huge, &[PRIMARY], PRIMARY).unwrap();
        assert_eq!(fit, PRIMARY);
        // A secondary primary offset is respected.
        let offset = Rect { x: -1920, y: 0, w: 1920, h: 1040 };
        let fit = recentred(win(5000, 0), &[offset], offset).unwrap();
        assert_eq!((fit.x, fit.y), (-1920 + 410, 160));
        // A visible window is left alone.
        assert_eq!(recentred(win(10, 10), &[PRIMARY], PRIMARY), None);
    }
}

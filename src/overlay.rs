use crate::config::Layout;
use crate::icons::Icon;
use std::cmp::max;
use x11rb::protocol::xproto::{Gcontext, Window};

pub struct WindowEntry {
    pub window: Window,
    pub title: String,
    pub icon: Option<Icon>,
}

pub struct OverlayWindow {
    pub window: Window,
    pub text_gc: Gcontext,
    pub selected_text_gc: Gcontext,
    pub highlight_gc: Gcontext,
    pub background_gc: Gcontext,
    pub icon_gc: Gcontext,
    pub width: u16,
    pub height: u16,
    pub layout: Layout,
    pub visible_capacity: usize,
}

pub struct OverlayState {
    pub windows: Vec<WindowEntry>,
    pub current: usize,
    pub first_visible: usize,
    pub overlay: OverlayWindow,
    pub alt_count: usize,
}

impl OverlayState {
    pub fn new(windows: Vec<WindowEntry>, overlay: OverlayWindow) -> Self {
        Self {
            windows,
            current: 0,
            first_visible: 0,
            overlay,
            alt_count: 0,
        }
    }

    pub fn advance(&mut self, direction: Direction) {
        if self.windows.is_empty() {
            return;
        }
        match direction {
            Direction::Forward => {
                self.current = (self.current + 1) % self.windows.len();
            }
            Direction::Backward => {
                if self.current == 0 {
                    self.current = self.windows.len() - 1;
                } else {
                    self.current -= 1;
                }
            }
        }
        self.ensure_visible();
    }

    pub fn ensure_visible(&mut self) {
        let capacity = max(1, self.overlay.visible_capacity);
        if self.current < self.first_visible {
            self.first_visible = self.current;
        } else if self.current >= self.first_visible + capacity {
            self.first_visible = self.current + 1 - capacity;
        }
    }

    pub fn visible_range(&self) -> impl Iterator<Item = usize> {
        let capacity = max(1, self.overlay.visible_capacity);
        let end = std::cmp::min(self.windows.len(), self.first_visible + capacity);
        self.first_visible..end
    }

    pub fn selected_window(&self) -> Option<Window> {
        self.windows.get(self.current).map(|entry| entry.window)
    }
}

#[derive(Copy, Clone)]
pub enum Direction {
    Forward,
    Backward,
}

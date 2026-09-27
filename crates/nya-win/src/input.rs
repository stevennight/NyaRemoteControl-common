//! Input injection with SendInput. Keys are injected as scancodes so the
//! result does not depend on either side's keyboard layout. Pressed keys and
//! buttons are tracked so they can all be released on disconnect.

use std::collections::HashSet;

use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// Rectangle of the streamed display in virtual-desktop pixels.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
pub struct Injector {
    rect: DisplayRect,
    keys: HashSet<(u16, bool)>,
    buttons: HashSet<Button>,
}

fn send(inputs: &[INPUT]) {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        tracing::debug!("SendInput injected {sent}/{} events", inputs.len());
    }
}

fn mouse_input(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

impl Injector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_display_rect(&mut self, rect: DisplayRect) {
        self.rect = rect;
    }

    pub fn key(&mut self, scancode: u16, extended: bool, down: bool) {
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scancode, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        };
        send(&[input]);
        if down {
            self.keys.insert((scancode, extended));
        } else {
            self.keys.remove(&(scancode, extended));
        }
    }

    /// `nx`, `ny` in 0..=65535 across the streamed display.
    pub fn mouse_abs(&mut self, nx: u32, ny: u32) {
        let r = self.rect;
        if r.width == 0 || r.height == 0 {
            return;
        }
        let px = r.left as i64 + (nx.min(65535) as i64 * (r.width as i64 - 1)) / 65535;
        let py = r.top as i64 + (ny.min(65535) as i64 * (r.height as i64 - 1)) / 65535;
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN) as i64,
                GetSystemMetrics(SM_YVIRTUALSCREEN) as i64,
                GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2) as i64,
                GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2) as i64,
            )
        };
        let ax = ((px - vx) * 65535 + (vw - 1) / 2) / (vw - 1);
        let ay = ((py - vy) * 65535 + (vh - 1) / 2) / (vh - 1);
        send(&[mouse_input(ax as i32, ay as i32, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK)]);
    }

    pub fn mouse_rel(&mut self, dx: i32, dy: i32) {
        send(&[mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE)]);
    }

    pub fn button(&mut self, b: Button, down: bool) {
        let (flags, data) = match (b, down) {
            (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (Button::X1, true) => (MOUSEEVENTF_XDOWN, 1),
            (Button::X1, false) => (MOUSEEVENTF_XUP, 1),
            (Button::X2, true) => (MOUSEEVENTF_XDOWN, 2),
            (Button::X2, false) => (MOUSEEVENTF_XUP, 2),
        };
        send(&[mouse_input(0, 0, data, flags)]);
        if down {
            self.buttons.insert(b);
        } else {
            self.buttons.remove(&b);
        }
    }

    /// Units of 1/120 notch.
    pub fn wheel(&mut self, dx: i32, dy: i32) {
        if dy != 0 {
            send(&[mouse_input(0, 0, dy, MOUSEEVENTF_WHEEL)]);
        }
        if dx != 0 {
            send(&[mouse_input(0, 0, dx, MOUSEEVENTF_HWHEEL)]);
        }
    }

    /// Release everything we pressed (disconnect, helper restart, focus loss).
    pub fn release_all(&mut self) {
        for (sc, ext) in std::mem::take(&mut self.keys) {
            self.key(sc, ext, false);
            self.keys.remove(&(sc, ext));
        }
        for b in std::mem::take(&mut self.buttons) {
            self.button(b, false);
            self.buttons.remove(&b);
        }
    }

    pub fn pressed_count(&self) -> usize {
        self.keys.len() + self.buttons.len()
    }
}

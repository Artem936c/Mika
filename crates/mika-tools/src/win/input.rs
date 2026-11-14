//! Keyboard and mouse emulation through `SendInput`.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

use crate::sys::{Input, MouseButton};

pub struct SendInputDevice;

/// Keys of the "extended" block: without the flag some programs read them as numpad keys.
fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C)
}

fn key(vk: u16, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn unicode(unit: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: the slice holds fully initialised INPUT structures.
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err("Windows не принял ввод: так бывает, когда впереди окно администратора".into())
    }
}

/// Shift, Ctrl, Alt, left Win, right Win.
const MODIFIER_KEYS: [u16; 5] = [0x10, 0x11, 0x12, 0x5B, 0x5C];

/// `GetAsyncKeyState` sets the high bit while the key is down.
fn is_down(state: i16) -> bool {
    state < 0
}

/// Whether the user holds Ctrl, Alt, Shift or Win now.
pub(super) fn modifiers_held() -> bool {
    MODIFIER_KEYS.iter().any(|vk| {
        // SAFETY: plain call that reads the state of one key.
        is_down(unsafe { GetAsyncKeyState(i32::from(*vk)) })
    })
}

/// A virtual key that no keyboard has and no program reacts to: 0xE8 is
/// "unassigned" in the Windows table of virtual-key codes.
const UNASSIGNED_KEY: u16 = 0xE8;

/// The one event of `nudge`: the release of the unassigned key.
fn nudge_input() -> INPUT {
    key(UNASSIGNED_KEY, true)
}

/// Sends one input event that changes nothing, so that this process is the
/// one the last input came from. False when Windows did not take the event.
///
/// Whether that gives the right to change the foreground window is not
/// promised anywhere: the documentation names "the process received the last
/// input event" and a press of Alt by the user. Programs are known to get
/// the right after any input they send themselves, and that is what this
/// relies on; it was not measured here (the ignored test
/// `the_input_way_alone_brings_a_window_forward` in `wnd.rs` measures it).
/// If it gives nothing, the way costs one look-around and the next one is tried.
///
/// Why this event is safe. It is the release of a key that was never pressed
/// and that stands for nothing: no character is typed, no shortcut and no
/// global hotkey starts on a release, nothing is left held. The usual trick, a
/// press of Alt, is not used: a lone Alt opens the menu bar of the program in
/// front, and Alt with a key the user is typing would become a shortcut.
/// The event is sent under the same mark as all her input (`own_input`), so
/// the watcher of events does not take it for the user being at the computer.
pub(super) fn nudge() -> bool {
    let _own = super::sensors::own_input();
    send(&[nudge_input()]).is_ok()
}

/// The sequence for one chord: every key down in order, then up in reverse.
pub fn chord_inputs(keys: &[u16]) -> Vec<(u16, bool)> {
    keys.iter()
        .map(|k| (*k, false))
        .chain(keys.iter().rev().map(|k| (*k, true)))
        .collect()
}

impl Input for SendInputDevice {
    fn chord(&self, keys: &[u16]) -> Result<(), String> {
        // Whoever built the chord: her own input never presses her own global
        // hotkey. What a hotkey does is the user's action and passes no gate.
        if let Some(combo) = crate::direct::own_hotkey_in(keys) {
            return Err(crate::direct::own_hotkey_refusal(&combo));
        }
        let _own = super::sensors::own_input();
        let inputs: Vec<INPUT> = chord_inputs(keys)
            .into_iter()
            .map(|(vk, up)| key(vk, up))
            .collect();
        // Noted before and after: a hotkey that fires around these presses
        // (a key of hers with Ctrl or Alt the user holds) is not the user's.
        crate::direct::keys_pressed();
        let sent = send(&inputs);
        crate::direct::keys_pressed();
        sent
    }

    fn text(&self, text: &str) -> Result<(), String> {
        let _own = super::sensors::own_input();
        let mut batch: Vec<INPUT> = Vec::new();
        for ch in text.chars() {
            match ch {
                '\r' => {}
                '\n' => {
                    batch.push(key(0x0D, false));
                    batch.push(key(0x0D, true));
                }
                '\t' => {
                    batch.push(key(0x09, false));
                    batch.push(key(0x09, true));
                }
                c => {
                    let mut units = [0u16; 2];
                    for unit in c.encode_utf16(&mut units) {
                        batch.push(unicode(*unit, false));
                        batch.push(unicode(*unit, true));
                    }
                }
            }
            // Small batches: a long text in one call can overflow the input queue of a slow program.
            if batch.len() >= 40 {
                send(&batch)?;
                batch.clear();
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
        }
        send(&batch)
    }

    fn click(&self, x: i32, y: i32, button: MouseButton, double: bool) -> Result<(), String> {
        let _own = super::sensors::own_input();
        super::physical_pixels();
        // SAFETY: plain call; a point off the screen is clamped by Windows.
        unsafe { SetCursorPos(x, y) }.map_err(|e| e.message())?;
        std::thread::sleep(std::time::Duration::from_millis(40));
        let (down, up) = match button {
            MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        };
        send(&[mouse(down, 0), mouse(up, 0)])?;
        if double {
            std::thread::sleep(std::time::Duration::from_millis(60));
            send(&[mouse(down, 0), mouse(up, 0)])?;
        }
        Ok(())
    }

    fn wheel(&self, x: i32, y: i32, lines: i32) -> Result<(), String> {
        let _own = super::sensors::own_input();
        super::physical_pixels();
        // SAFETY: plain call.
        unsafe { SetCursorPos(x, y) }.map_err(|e| e.message())?;
        std::thread::sleep(std::time::Duration::from_millis(40));
        // One wheel notch is 120; a positive amount scrolls up, so "down" is negative.
        send(&[mouse(MOUSEEVENTF_WHEEL, -lines * 120)])
    }

    fn modifiers_down(&self) -> bool {
        modifiers_held()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_presses_in_order_and_releases_in_reverse() {
        assert_eq!(
            chord_inputs(&[0x11, 0x10, 0x53]),
            vec![
                (0x11, false),
                (0x10, false),
                (0x53, false),
                (0x53, true),
                (0x10, true),
                (0x11, true)
            ]
        );
    }

    #[test]
    fn a_held_key_is_told_by_the_high_bit() {
        // Down now; pressed since the last call but up now; untouched.
        assert!(is_down(i16::MIN) && is_down(i16::MIN | 1));
        assert!(!is_down(1) && !is_down(0));
        // VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN.
        assert_eq!(MODIFIER_KEYS, [0x10, 0x11, 0x12, 0x5B, 0x5C]);
    }

    #[test]
    fn the_nudge_is_one_release_of_a_key_that_stands_for_nothing() {
        // Built only, never sent: a test does not type into the live desktop.
        let input = nudge_input();
        assert_eq!(input.r#type, INPUT_KEYBOARD);
        // SAFETY: the union was filled as a keyboard event just above.
        let ki = unsafe { input.Anonymous.ki };
        assert_eq!(ki.wVk, VIRTUAL_KEY(0xE8));
        assert_eq!(ki.dwFlags, KEYEVENTF_KEYUP, "a release, never a press");
        assert_eq!(ki.wScan, 0);
        assert!(!MODIFIER_KEYS.contains(&UNASSIGNED_KEY));
    }

    #[test]
    fn arrows_and_delete_are_extended_keys() {
        assert!(is_extended(0x25) && is_extended(0x2E) && is_extended(0x5B));
        assert!(!is_extended(0x41) && !is_extended(0x0D));
    }
}

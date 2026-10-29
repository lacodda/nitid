//! Which presses of the space bar ask for a look.
//!
//! The keyboard is read with a low-level hook (`WH_KEYBOARD_LL`), the one way
//! a program sees a key pressed in another program's window without taking
//! it away from that program. The press is never swallowed: Explorer still
//! gets its space bar and does with it what it always did, which on a
//! selected file is nothing. Deciding to swallow it would mean asking Explorer
//! what is selected *inside* the hook, and a hook that waits on another
//! process is one Windows unhooks without a word once it has been slow a few
//! times (the `LowLevelHooksTimeout` rule).
//!
//! So the hook decides only what it can decide from the keys themselves —
//! a bare space, pressed rather than held, not in the middle of typing — and
//! posts that to the resident's window. Whether the window in front is
//! Explorer, and what it has selected, is asked there (`explorer`).
//!
//! The hook runs on a thread of its own with nothing else to do, for the
//! reason ADR 0038 gives the global key its own thread: a hook is answered
//! from its thread's message loop, and a loop that is also busy talking to
//! Explorer is a hook that is late.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SPACE};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// How long after a typed character a space still belongs to the typing.
///
/// Explorer's list finds a file by its name as it is typed — "my photo" goes
/// to the first file called that — and the space in it is a space bar like any
/// other. A second is about as long as the list keeps a word going; a space
/// later than that starts nothing in the list, and is a look.
pub const TYPING: Duration = Duration::from_secs(1);

/// What a key did, reduced to what decides a look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stroke {
    /// The space bar went down. `chord` when a modifier was held with it:
    /// `Ctrl+Space` is Explorer's own, and toggles the selection.
    SpaceDown {
        chord: bool,
    },
    SpaceUp,
    /// A key that types a character into the list's type-to-find.
    Typed,
    Other,
}

/// What the hook remembers between strokes.
#[derive(Debug, Default)]
pub struct Watch {
    /// When the last character was typed, for the typing a space can belong
    /// to.
    typed_at: Option<Instant>,
    /// Whether the space bar is held: a held key repeats its press, and one
    /// look is one press.
    space_held: bool,
}

impl Watch {
    /// Whether this stroke asks for a look.
    pub fn look(&mut self, stroke: Stroke, now: Instant) -> bool {
        match stroke {
            Stroke::SpaceDown { chord } => {
                let repeat = std::mem::replace(&mut self.space_held, true);
                let typing = self.typed_at.is_some_and(|at| now.saturating_duration_since(at) < TYPING);
                if typing {
                    // The space is part of the name being typed, and the name
                    // goes on after it: "my photo 2" is one word to the list.
                    self.typed_at = Some(now);
                }
                !repeat && !chord && !typing
            }
            Stroke::SpaceUp => {
                self.space_held = false;
                false
            }
            Stroke::Typed => {
                self.typed_at = Some(now);
                false
            }
            Stroke::Other => false,
        }
    }
}

/// Which modifiers are held, as the hook can tell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

/// Name a key the way `Watch` reads it.
///
/// Typing is a character key — a letter, a digit, punctuation, the number
/// pad — pressed with nothing held but Shift, which only changes the
/// character. With `Ctrl`, `Alt` or `Win` it is a command, and types nothing.
pub fn stroke(key: u32, down: bool, held: Held) -> Stroke {
    if key == u32::from(VK_SPACE.0) {
        return if down {
            Stroke::SpaceDown {
                chord: held.ctrl || held.alt || held.shift || held.win,
            }
        } else {
            Stroke::SpaceUp
        };
    }
    let character = matches!(key, 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x6F | 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2);
    if down && character && !(held.ctrl || held.alt || held.win) {
        Stroke::Typed
    } else {
        Stroke::Other
    }
}

/// The message the hook posts to the resident's window when a press asks for
/// a look.
pub const WM_LOOK: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// The hook, while it is installed: dropping this takes it off.
pub struct Listening {
    thread: u32,
    joined: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        // The thread made its queue before reporting in, so the quit is not
        // lost on a thread that has none yet.
        unsafe {
            let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(joined) = self.joined.take() {
            let _ = joined.join();
        }
    }
}

thread_local! {
    /// The hook's state, on the hook's own thread: a low-level hook is called
    /// on the thread that installed it, so nothing else ever touches it.
    static HOOKED: RefCell<Option<(Watch, isize)>> = const { RefCell::new(None) };
}

/// Install the hook on a thread of its own, posting looks to `window`.
///
/// Answered before returning: a hook Windows refused is said now, rather than
/// found out as a space bar that does nothing.
pub fn listen(window: HWND) -> Result<Listening> {
    let target = window.0 as isize;
    let (report, answer) = std::sync::mpsc::channel();
    let joined = std::thread::Builder::new()
        .name("nitid-keys".into())
        .spawn(move || {
            // The queue first: `PostThreadMessageW` to a thread without one is
            // dropped, and the quit that ends this loop would never arrive.
            let mut message = MSG::default();
            unsafe {
                let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
            }
            HOOKED.with(|hooked| *hooked.borrow_mut() = Some((Watch::default(), target)));

            let module = unsafe { GetModuleHandleW(None) }.ok();
            let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hooked), module.map(Into::into), 0) } {
                Ok(hook) => hook,
                Err(error) => {
                    let _ = report.send(Err(error));
                    return;
                }
            };
            let _ = report.send(Ok(unsafe { GetCurrentThreadId() }));

            while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        })
        .context("starting the keyboard thread")?;

    match answer.recv() {
        Ok(Ok(thread)) => Ok(Listening { thread, joined: Some(joined) }),
        Ok(Err(error)) => Err(anyhow::Error::new(error).context("listening to the keyboard")),
        Err(_) => bail!("the keyboard thread ended before it could listen"),
    }
}

/// Whether a modifier key is down right now.
fn down(key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    // The high bit is "down"; the low bit says something else entirely.
    (unsafe { GetAsyncKeyState(i32::from(key.0)) } as u16 & 0x8000) != 0
}

/// The hook itself. Kept to reading the stroke and posting: anything slow
/// here is a keyboard that lags in every program on the machine.
unsafe extern "system" fn hooked(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let message = wparam.0 as u32;
        let down_now = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up_now = matches!(message, WM_KEYUP | WM_SYSKEYUP);
        if down_now || up_now {
            // SAFETY: for `HC_ACTION` Windows passes a pointer to this struct.
            let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) }.vkCode;
            let held = Held {
                ctrl: down(VK_CONTROL),
                alt: down(VK_MENU),
                shift: down(VK_SHIFT),
                win: down(VK_LWIN) || down(VK_RWIN),
            };
            let stroke = stroke(key, down_now, held);
            HOOKED.with(|hooked| {
                if let Some((watch, target)) = hooked.borrow_mut().as_mut()
                    && watch.look(stroke, Instant::now())
                {
                    unsafe {
                        let _ = PostMessageW(Some(HWND(*target as *mut _)), WM_LOOK, WPARAM(0), LPARAM(0));
                    }
                }
            });
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: u32 = 0x20;
    const A: u32 = 0x41;

    fn bare() -> Held {
        Held::default()
    }

    /// The whole feature: a bare space, pressed in a list nobody is typing
    /// into, is a look — and only the press, not the repeats of a held key.
    #[test]
    fn a_bare_press_is_one_look() {
        let mut watch = Watch::default();
        let now = Instant::now();
        assert!(watch.look(stroke(SPACE, true, bare()), now), "a bare space was not a look");
        // Held: the keyboard repeats the press, and each repeat would open
        // and close the window in turn.
        assert!(!watch.look(stroke(SPACE, true, bare()), now + Duration::from_millis(500)));
        assert!(!watch.look(stroke(SPACE, false, bare()), now + Duration::from_millis(600)));
        assert!(
            watch.look(stroke(SPACE, true, bare()), now + Duration::from_millis(700)),
            "the next press was lost"
        );
    }

    /// `Ctrl+Space` toggles Explorer's selection, `Shift+Space` extends it:
    /// both are Explorer's, not a look.
    #[test]
    fn a_space_with_a_modifier_is_explorers() {
        for held in [
            Held { ctrl: true, ..bare() },
            Held { alt: true, ..bare() },
            Held { shift: true, ..bare() },
            Held { win: true, ..bare() },
        ] {
            let mut watch = Watch::default();
            assert!(
                !watch.look(stroke(SPACE, true, held), Instant::now()),
                "{held:?} with space was taken for a look"
            );
        }
    }

    /// Typing "my photo" to find a file must not open the file the list was
    /// on when the space came — and the typing goes on past the space.
    #[test]
    fn a_space_while_typing_a_name_belongs_to_the_name() {
        let mut watch = Watch::default();
        let start = Instant::now();
        watch.look(stroke(A, true, bare()), start);
        assert!(
            !watch.look(stroke(SPACE, true, bare()), start + Duration::from_millis(200)),
            "a space in typing was a look"
        );
        watch.look(stroke(SPACE, false, bare()), start + Duration::from_millis(250));
        // A second space soon after the first is still the same name: the
        // space itself kept the typing going.
        assert!(!watch.look(stroke(SPACE, true, bare()), start + Duration::from_millis(1100)));
        watch.look(stroke(SPACE, false, bare()), start + Duration::from_millis(1150));
        // Once the list has given up on the word, a space is a look again.
        assert!(watch.look(stroke(SPACE, true, bare()), start + Duration::from_millis(1100) + TYPING));
    }

    /// Only characters count as typing: an arrow key moving the selection
    /// and then a space is the most ordinary look there is.
    #[test]
    fn moving_the_selection_is_not_typing() {
        let mut watch = Watch::default();
        let now = Instant::now();
        const DOWN: u32 = 0x28;
        assert_eq!(stroke(DOWN, true, bare()), Stroke::Other);
        watch.look(stroke(DOWN, true, bare()), now);
        assert!(watch.look(stroke(SPACE, true, bare()), now + Duration::from_millis(50)));
    }

    /// A letter with `Ctrl` is a command — `Ctrl+A` selects everything — and
    /// one with Shift is still a letter.
    #[test]
    fn a_letter_with_a_command_modifier_types_nothing() {
        assert_eq!(stroke(A, true, Held { ctrl: true, ..bare() }), Stroke::Other);
        assert_eq!(stroke(A, true, Held { shift: true, ..bare() }), Stroke::Typed);
        assert_eq!(stroke(A, false, bare()), Stroke::Other, "a key coming up is not a character typed");
    }
}

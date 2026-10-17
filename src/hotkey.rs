//! A key that reaches the viewer from anywhere in Windows.
//!
//! Pressed in any program, it brings the viewer forward with the newest
//! picture in the folders it has been to: the screenshot just taken, the frame
//! a generator just wrote. That is a small use for a large mechanism, on
//! purpose — the screenshot tool of 2.x starts from a key pressed in another
//! program, and this is that key, worn in on something that cannot do harm.
//!
//! **It answers while the viewer is open**, minimised or behind other windows.
//! A viewer that has been closed is a process that has ended, and nothing of
//! it is left to listen; staying behind after the window closes is a resident
//! process, which is the capture tool's to introduce, with its own way of
//! being told to go.
//!
//! The key is registered with `RegisterHotKey` on a thread of its own with its
//! own message loop, rather than through the window's: the window's loop is
//! winit's, and reaching into it would tie this to how winit dispatches thread
//! messages. A thread that owns its registration is also exactly what a
//! resident process will need — it has no window to give one to.

use std::fmt;

/// Which modifiers a combination holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A combination of keys, as the settings name it and Windows registers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    /// The Windows virtual-key code of the key held with them.
    pub key: u32,
}

impl Default for Hotkey {
    /// `Win+Alt+N`.
    ///
    /// Not `Ctrl+Alt` and a letter, the combination most programs reach for:
    /// on the European layouts `Ctrl+Alt` *is* `AltGr`, and a key taken
    /// globally there is a letter nobody can type any more — `Ctrl+Alt+N` is
    /// the Polish ń. `Win` with `Alt` is free of that, and `N` is for nitid
    /// and for "newest". Windows itself answers no `Win+Alt+N`.
    fn default() -> Self {
        Self {
            modifiers: Modifiers {
                win: true,
                alt: true,
                ..Modifiers::default()
            },
            key: u32::from(b'N'),
        }
    }
}

/// The keys a combination can end in, by name and virtual-key code.
///
/// Letters and digits are their own codes and are not listed. The function
/// keys run up to F24, which no keyboard on a desk has: they are what macro
/// pads and remapping tools send, which makes them the safest keys of all to
/// take globally.
const NAMED: &[(&str, u32)] = &[
    ("Space", 0x20),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Insert", 0x2D),
    ("Pause", 0x13),
    ("PrintScreen", 0x2C),
];

/// The function keys: F1 is 0x70, and the rest follow it.
const F1: u32 = 0x70;
const FUNCTION_KEYS: u32 = 24;

impl Hotkey {
    /// Read a combination as a person writes it: `Win+Alt+N`,
    /// `ctrl + shift + f9`. Case and spaces do not matter, and the modifiers
    /// may come in any order; the key comes last.
    ///
    /// Refused unless it holds `Ctrl`, `Alt` or `Win` — or is a key nobody
    /// types with (F13 and up, Pause, Print Screen). A bare letter, or one
    /// with only `Shift`, taken from the whole of Windows would be a letter
    /// gone from every program on the machine.
    pub fn parse(text: &str) -> Result<Self, String> {
        let parts: Vec<String> = text.split('+').map(|part| part.trim().to_ascii_lowercase()).collect();
        let Some((last, held)) = parts.split_last() else {
            return Err("name a key".to_string());
        };

        let mut modifiers = Modifiers::default();
        for part in held {
            let slot = match part.as_str() {
                "win" | "windows" => &mut modifiers.win,
                "ctrl" | "control" => &mut modifiers.ctrl,
                "alt" => &mut modifiers.alt,
                "shift" => &mut modifiers.shift,
                "" => return Err("a + with nothing beside it".to_string()),
                other => return Err(format!("{other} is not Win, Ctrl, Alt or Shift")),
            };
            if *slot {
                return Err(format!("{part} is named twice"));
            }
            *slot = true;
        }

        let key = key_code(last).ok_or_else(|| match last.as_str() {
            "" => "name the key after the last +".to_string(),
            other => format!("{other} is not a key nitid can listen for"),
        })?;

        let hotkey = Self { modifiers, key };
        if !(modifiers.win || modifiers.ctrl || modifiers.alt || hotkey.untyped()) {
            return Err("hold Win, Ctrl or Alt with it, or every program loses that key".to_string());
        }
        Ok(hotkey)
    }

    /// Whether the key is one nobody types text with, and so may be taken on
    /// its own.
    fn untyped(&self) -> bool {
        self.key == 0x13 || self.key == 0x2C || (F1 + 12..F1 + FUNCTION_KEYS).contains(&self.key)
    }
}

/// The virtual-key code a key's name stands for.
fn key_code(name: &str) -> Option<u32> {
    let bytes = name.as_bytes();
    if let [single] = bytes
        && single.is_ascii_alphanumeric()
    {
        return Some(u32::from(single.to_ascii_uppercase()));
    }
    if let Some(number) = name.strip_prefix('f').and_then(|digits| digits.parse::<u32>().ok())
        && (1..=FUNCTION_KEYS).contains(&number)
    {
        return Some(F1 + number - 1);
    }
    NAMED.iter().find(|(named, _)| named.eq_ignore_ascii_case(name)).map(|(_, code)| *code)
}

/// The name a virtual-key code is written with.
fn key_name(code: u32) -> String {
    match code {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(code).map(String::from).unwrap_or_default(),
        _ if (F1..F1 + FUNCTION_KEYS).contains(&code) => format!("F{}", code - F1 + 1),
        _ => NAMED
            .iter()
            .find(|(_, named)| *named == code)
            .map(|(name, _)| (*name).to_string())
            .unwrap_or_else(|| format!("key {code:#04x}")),
    }
}

impl fmt::Display for Hotkey {
    /// Written the way Windows writes its own: the Windows key first, then
    /// `Ctrl`, `Alt` and `Shift`, then the key — so whatever order it was
    /// typed in, it reads back the same.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let held = [
            (self.modifiers.win, "Win"),
            (self.modifiers.ctrl, "Ctrl"),
            (self.modifiers.alt, "Alt"),
            (self.modifiers.shift, "Shift"),
        ];
        for (on, name) in held {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key_name(self.key))
    }
}

/// Why a combination could not be listened for.
#[derive(Debug)]
pub enum Refused {
    /// Another program holds it. The person has to choose another, and the
    /// viewer has to say so: a key that silently does nothing is a key that
    /// gets pressed harder.
    Taken,
    /// Windows refused for some other reason, or there is no Windows here.
    Failed(String),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Taken => f.write_str("another program already holds it"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

#[cfg(windows)]
pub use windows_listener::{Listening, listen};

#[cfg(windows)]
mod windows_listener {
    use std::sync::mpsc;
    use std::thread;

    use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT, WM_USER};

    use super::{Hotkey, Refused};

    /// The registration's own number. Registrations are per thread, and this
    /// thread holds only the one.
    const ID: i32 = 1;

    /// A combination being listened for. Dropping it lets go of it.
    pub struct Listening {
        thread: u32,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl Listening {
        /// Deliver a press as if the key had been pressed — for the test that
        /// the press reaches `on_press`, which cannot press a key that belongs
        /// to the whole desktop it runs on.
        #[cfg(test)]
        pub fn press(&self) {
            // SAFETY: posting to a thread id this value owns; the thread runs
            // a message loop until this value is dropped.
            unsafe {
                let _ = PostThreadMessageW(self.thread, WM_HOTKEY, WPARAM(ID as usize), LPARAM(0));
            }
        }
    }

    /// Listen for `hotkey` anywhere in Windows, calling `on_press` on the
    /// listening thread each time it is pressed.
    ///
    /// Returns once the registration has been answered, so a combination
    /// another program holds is known to the caller at once rather than found
    /// out by a key that never does anything.
    pub fn listen(hotkey: Hotkey, on_press: impl Fn() + Send + 'static) -> Result<Listening, Refused> {
        let (answer, answered) = mpsc::channel::<Result<u32, Refused>>();

        let handle = thread::spawn(move || {
            let mut message = MSG::default();
            // SAFETY: plain Win32 calls on this thread's own queue and its own
            // registration, made and released here.
            unsafe {
                // A thread has no message queue until it asks for one, and a
                // `WM_QUIT` posted before the queue exists is lost — the
                // listener would then never stop. Peeking makes the queue.
                let _ = PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE);

                if let Err(error) = RegisterHotKey(None, ID, modifiers(&hotkey) | MOD_NOREPEAT, hotkey.key) {
                    let refused = if error.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
                        Refused::Taken
                    } else {
                        Refused::Failed(error.message())
                    };
                    let _ = answer.send(Err(refused));
                    return;
                }
                let _ = answer.send(Ok(GetCurrentThreadId()));

                // `GetMessageW` returns zero for `WM_QUIT`, and minus one for
                // a failure that would otherwise spin.
                while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    if message.message == WM_HOTKEY && message.wParam.0 == ID as usize {
                        on_press();
                    }
                }
                let _ = UnregisterHotKey(None, ID);
            }
        });

        match answered.recv() {
            Ok(Ok(thread)) => Ok(Listening { thread, handle: Some(handle) }),
            Ok(Err(refused)) => {
                let _ = handle.join();
                Err(refused)
            }
            Err(_) => Err(Refused::Failed("the listening thread ended before answering".to_string())),
        }
    }

    impl Drop for Listening {
        fn drop(&mut self) {
            // SAFETY: the thread id is the listener's, whose loop ends on
            // `WM_QUIT` and releases the registration on the way out.
            unsafe {
                let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn modifiers(hotkey: &Hotkey) -> HOT_KEY_MODIFIERS {
        let mut held = HOT_KEY_MODIFIERS(0);
        for (on, flag) in [
            (hotkey.modifiers.win, MOD_WIN),
            (hotkey.modifiers.ctrl, MOD_CONTROL),
            (hotkey.modifiers.alt, MOD_ALT),
            (hotkey.modifiers.shift, MOD_SHIFT),
        ] {
            if on {
                held |= flag;
            }
        }
        held
    }
}

/// Elsewhere there is no `RegisterHotKey`, and the viewer says so rather than
/// pretending to listen.
#[cfg(not(windows))]
pub struct Listening;

#[cfg(not(windows))]
pub fn listen(_hotkey: Hotkey, _on_press: impl Fn() + Send + 'static) -> Result<Listening, Refused> {
    Err(Refused::Failed("a key held across the whole desktop is a Windows feature".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_win_alt_n() {
        assert_eq!(Hotkey::default().to_string(), "Win+Alt+N");
        assert_eq!(Hotkey::parse("Win+Alt+N"), Ok(Hotkey::default()));
    }

    /// However it was typed, a combination reads back one way.
    #[test]
    fn a_combination_reads_back_the_same_whatever_its_order_or_case() {
        let typed = Hotkey::parse(" shift + ctrl+F9 ").unwrap();
        assert_eq!(typed.to_string(), "Ctrl+Shift+F9");
        assert_eq!(Hotkey::parse(&typed.to_string()), Ok(typed));
        assert_eq!(Hotkey::parse("alt+win+n"), Ok(Hotkey::default()));
    }

    /// Every key the parser takes is one it can write back, and the codes are
    /// the ones Windows uses — a function key off by one would register a
    /// different key from the one the settings show.
    #[test]
    fn the_keys_are_the_codes_windows_uses() {
        assert_eq!(key_code("a"), Some(0x41));
        assert_eq!(key_code("7"), Some(0x37));
        assert_eq!(key_code("f1"), Some(0x70));
        assert_eq!(key_code("f12"), Some(0x7B));
        assert_eq!(key_code("f24"), Some(0x87));
        assert_eq!(key_code("f25"), None);
        assert_eq!(key_code("f0"), None);
        assert_eq!(key_code("printscreen"), Some(0x2C));
        for code in (0x30..=0x39).chain(0x41..=0x5A).chain(0x70..=0x87).chain(NAMED.iter().map(|(_, code)| *code)) {
            assert_eq!(key_code(&key_name(code).to_ascii_lowercase()), Some(code), "{code:#x} does not read back");
        }
    }

    /// A key taken from the whole desktop without a modifier is a key gone
    /// from every program — unless nobody types with it.
    #[test]
    fn a_key_every_program_types_with_is_refused_without_a_modifier() {
        assert!(Hotkey::parse("N").is_err());
        assert!(Hotkey::parse("Shift+N").is_err(), "Shift alone would take the capital away");
        assert!(Hotkey::parse("F5").is_err());
        assert!(Hotkey::parse("Space").is_err());
        assert!(Hotkey::parse("Ctrl+N").is_ok());
        assert!(Hotkey::parse("F13").is_ok());
        assert!(Hotkey::parse("F24").is_ok());
        assert!(Hotkey::parse("PrintScreen").is_ok());
        assert!(Hotkey::parse("Pause").is_ok());
    }

    #[test]
    fn nonsense_is_refused_with_a_reason() {
        for text in ["", "Win+", "+N", "Win+Win+N", "Hyper+N", "Win+Alt+Banana", "Ctrl++N"] {
            let refused = Hotkey::parse(text);
            assert!(refused.as_ref().is_err_and(|why| !why.is_empty()), "{text:?} was taken as {refused:?}");
        }
    }

    /// The combination reaches the code that answers it. Pressed by posting
    /// the message Windows would post: a test cannot press a key that belongs
    /// to the whole desktop it runs on.
    #[cfg(windows)]
    #[test]
    fn a_press_reaches_the_listener() {
        use std::sync::mpsc;
        use std::time::Duration;

        // A combination nobody holds: every modifier and a key no keyboard has.
        let rare = Hotkey::parse("Win+Ctrl+Alt+Shift+F23").unwrap();
        let (sender, pressed) = mpsc::channel();
        let listening = listen(rare, move || {
            let _ = sender.send(());
        })
        .expect("a combination nobody holds was refused");

        listening.press();
        assert!(pressed.recv_timeout(Duration::from_secs(5)).is_ok(), "the press never reached the listener");
    }

    /// A combination already held is reported as held, not as silence, and
    /// letting go of it frees it for the next listener.
    #[cfg(windows)]
    #[test]
    fn a_held_combination_is_refused_until_it_is_let_go() {
        let rare = Hotkey::parse("Win+Ctrl+Alt+Shift+F22").unwrap();
        let first = listen(rare, || {}).expect("a combination nobody holds was refused");

        match listen(rare, || {}) {
            Err(Refused::Taken) => {}
            other => panic!("a held combination was answered with {:?}", other.map(|_| ())),
        }

        drop(first);
        assert!(listen(rare, || {}).is_ok(), "a combination let go of was still held");
    }
}

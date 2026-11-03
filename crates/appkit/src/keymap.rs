use sdl3::keycodes::{keymod, scancode};

/// `kVK_*` from `<Carbon/HIToolbox/Events.h>`, for the keys this tree names.
///
/// Apple's header is the only place these numbers exist; a Linux host has no
/// copy of it to generate them from, so they are transcribed - which is why the
/// tests below check the ones a person is most likely to have got wrong.
#[rustfmt::skip]
pub mod vk {
    pub const ANSI_A: u16 = 0x00; pub const ANSI_S: u16 = 0x01;
    pub const ANSI_D: u16 = 0x02; pub const ANSI_F: u16 = 0x03;
    pub const ANSI_H: u16 = 0x04; pub const ANSI_G: u16 = 0x05;
    pub const ANSI_Z: u16 = 0x06; pub const ANSI_X: u16 = 0x07;
    pub const ANSI_C: u16 = 0x08; pub const ANSI_V: u16 = 0x09;
    pub const ISO_SECTION: u16 = 0x0A;
    pub const ANSI_B: u16 = 0x0B; pub const ANSI_Q: u16 = 0x0C;
    pub const ANSI_W: u16 = 0x0D; pub const ANSI_E: u16 = 0x0E;
    pub const ANSI_R: u16 = 0x0F; pub const ANSI_Y: u16 = 0x10;
    pub const ANSI_T: u16 = 0x11;
    pub const ANSI_1: u16 = 0x12; pub const ANSI_2: u16 = 0x13;
    pub const ANSI_3: u16 = 0x14; pub const ANSI_4: u16 = 0x15;
    pub const ANSI_6: u16 = 0x16; pub const ANSI_5: u16 = 0x17;
    pub const ANSI_EQUAL: u16 = 0x18;
    pub const ANSI_9: u16 = 0x19; pub const ANSI_7: u16 = 0x1A;
    pub const ANSI_MINUS: u16 = 0x1B;
    pub const ANSI_8: u16 = 0x1C; pub const ANSI_0: u16 = 0x1D;
    pub const ANSI_RIGHT_BRACKET: u16 = 0x1E;
    pub const ANSI_O: u16 = 0x1F; pub const ANSI_U: u16 = 0x20;
    pub const ANSI_LEFT_BRACKET: u16 = 0x21;
    pub const ANSI_I: u16 = 0x22; pub const ANSI_P: u16 = 0x23;
    pub const RETURN: u16 = 0x24;
    pub const ANSI_L: u16 = 0x25; pub const ANSI_J: u16 = 0x26;
    pub const ANSI_QUOTE: u16 = 0x27;
    pub const ANSI_K: u16 = 0x28;
    pub const ANSI_SEMICOLON: u16 = 0x29;
    pub const ANSI_BACKSLASH: u16 = 0x2A;
    pub const ANSI_COMMA: u16 = 0x2B;
    pub const ANSI_SLASH: u16 = 0x2C;
    pub const ANSI_N: u16 = 0x2D; pub const ANSI_M: u16 = 0x2E;
    pub const ANSI_PERIOD: u16 = 0x2F;
    pub const TAB: u16 = 0x30;
    pub const SPACE: u16 = 0x31;
    pub const ANSI_GRAVE: u16 = 0x32;
    pub const DELETE: u16 = 0x33;
    pub const ESCAPE: u16 = 0x35;
    pub const RIGHT_COMMAND: u16 = 0x36;
    pub const COMMAND: u16 = 0x37;
    pub const SHIFT: u16 = 0x38;
    pub const CAPS_LOCK: u16 = 0x39;
    pub const OPTION: u16 = 0x3A;
    pub const CONTROL: u16 = 0x3B;
    pub const RIGHT_SHIFT: u16 = 0x3C;
    pub const RIGHT_OPTION: u16 = 0x3D;
    pub const RIGHT_CONTROL: u16 = 0x3E;
    pub const F17: u16 = 0x40;
    pub const ANSI_KEYPAD_DECIMAL: u16 = 0x41;
    pub const ANSI_KEYPAD_MULTIPLY: u16 = 0x43;
    pub const ANSI_KEYPAD_PLUS: u16 = 0x45;
    pub const ANSI_KEYPAD_CLEAR: u16 = 0x47;
    pub const VOLUME_UP: u16 = 0x48;
    pub const VOLUME_DOWN: u16 = 0x49;
    pub const MUTE: u16 = 0x4A;
    pub const ANSI_KEYPAD_DIVIDE: u16 = 0x4B;
    pub const KEYPAD_ENTER: u16 = 0x4C;
    pub const ANSI_KEYPAD_MINUS: u16 = 0x4E;
    pub const F18: u16 = 0x4F; pub const F19: u16 = 0x50;
    pub const ANSI_KEYPAD_EQUALS: u16 = 0x51;
    pub const ANSI_KEYPAD_0: u16 = 0x52; pub const ANSI_KEYPAD_1: u16 = 0x53;
    pub const ANSI_KEYPAD_2: u16 = 0x54; pub const ANSI_KEYPAD_3: u16 = 0x55;
    pub const ANSI_KEYPAD_4: u16 = 0x56; pub const ANSI_KEYPAD_5: u16 = 0x57;
    pub const ANSI_KEYPAD_6: u16 = 0x58; pub const ANSI_KEYPAD_7: u16 = 0x59;
    pub const F20: u16 = 0x5A;
    pub const ANSI_KEYPAD_8: u16 = 0x5B; pub const ANSI_KEYPAD_9: u16 = 0x5C;
    pub const JIS_YEN: u16 = 0x5D;
    pub const JIS_UNDERSCORE: u16 = 0x5E;
    pub const F5: u16 = 0x60; pub const F6: u16 = 0x61;
    pub const F7: u16 = 0x62; pub const F3: u16 = 0x63;
    pub const F8: u16 = 0x64; pub const F9: u16 = 0x65;
    pub const JIS_EISU: u16 = 0x66;
    pub const F11: u16 = 0x67;
    pub const JIS_KANA: u16 = 0x68;
    pub const F13: u16 = 0x69; pub const F16: u16 = 0x6A;
    pub const F14: u16 = 0x6B;
    pub const F10: u16 = 0x6D; pub const F12: u16 = 0x6F;
    pub const F15: u16 = 0x71;
    pub const HELP: u16 = 0x72;
    pub const HOME: u16 = 0x73;
    pub const PAGE_UP: u16 = 0x74;
    pub const FORWARD_DELETE: u16 = 0x75;
    pub const F4: u16 = 0x76;
    pub const END: u16 = 0x77;
    pub const F2: u16 = 0x78;
    pub const PAGE_DOWN: u16 = 0x79;
    pub const F1: u16 = 0x7A;
    pub const LEFT_ARROW: u16 = 0x7B; pub const RIGHT_ARROW: u16 = 0x7C;
    pub const DOWN_ARROW: u16 = 0x7D; pub const UP_ARROW: u16 = 0x7E;
}

/// `SDL_Scancode` -> `kVK_*`, for every key a keyboard has that this tree can
/// name. A scancode absent from it has no macOS code, and the event carries
/// [`UNKNOWN`] rather than a neighbour's.
static VIRTUAL_KEYS: &[(u32, u16)] = &[
    (scancode::A, vk::ANSI_A),
    (scancode::B, vk::ANSI_B),
    (scancode::C, vk::ANSI_C),
    (scancode::D, vk::ANSI_D),
    (scancode::E, vk::ANSI_E),
    (scancode::F, vk::ANSI_F),
    (scancode::G, vk::ANSI_G),
    (scancode::H, vk::ANSI_H),
    (scancode::I, vk::ANSI_I),
    (scancode::J, vk::ANSI_J),
    (scancode::K, vk::ANSI_K),
    (scancode::L, vk::ANSI_L),
    (scancode::M, vk::ANSI_M),
    (scancode::N, vk::ANSI_N),
    (scancode::O, vk::ANSI_O),
    (scancode::P, vk::ANSI_P),
    (scancode::Q, vk::ANSI_Q),
    (scancode::R, vk::ANSI_R),
    (scancode::S, vk::ANSI_S),
    (scancode::T, vk::ANSI_T),
    (scancode::U, vk::ANSI_U),
    (scancode::V, vk::ANSI_V),
    (scancode::W, vk::ANSI_W),
    (scancode::X, vk::ANSI_X),
    (scancode::Y, vk::ANSI_Y),
    (scancode::Z, vk::ANSI_Z),
    (scancode::KEY_1, vk::ANSI_1),
    (scancode::KEY_2, vk::ANSI_2),
    (scancode::KEY_3, vk::ANSI_3),
    (scancode::KEY_4, vk::ANSI_4),
    (scancode::KEY_5, vk::ANSI_5),
    (scancode::KEY_6, vk::ANSI_6),
    (scancode::KEY_7, vk::ANSI_7),
    (scancode::KEY_8, vk::ANSI_8),
    (scancode::KEY_9, vk::ANSI_9),
    (scancode::KEY_0, vk::ANSI_0),
    (scancode::RETURN, vk::RETURN),
    (scancode::ESCAPE, vk::ESCAPE),
    (scancode::BACKSPACE, vk::DELETE),
    (scancode::TAB, vk::TAB),
    (scancode::SPACE, vk::SPACE),
    (scancode::MINUS, vk::ANSI_MINUS),
    (scancode::EQUALS, vk::ANSI_EQUAL),
    (scancode::LEFTBRACKET, vk::ANSI_LEFT_BRACKET),
    (scancode::RIGHTBRACKET, vk::ANSI_RIGHT_BRACKET),
    (scancode::BACKSLASH, vk::ANSI_BACKSLASH),
    (scancode::SEMICOLON, vk::ANSI_SEMICOLON),
    (scancode::APOSTROPHE, vk::ANSI_QUOTE),
    (scancode::GRAVE, vk::ANSI_GRAVE),
    (scancode::COMMA, vk::ANSI_COMMA),
    (scancode::PERIOD, vk::ANSI_PERIOD),
    (scancode::SLASH, vk::ANSI_SLASH),
    (scancode::CAPSLOCK, vk::CAPS_LOCK),
    // The key left of `Z` on an ISO keyboard, which is a separate key there and
    // not the backslash it shares a legend with.
    (scancode::NONUSBACKSLASH, vk::ISO_SECTION),
    (scancode::F1, vk::F1),
    (scancode::F2, vk::F2),
    (scancode::F3, vk::F3),
    (scancode::F4, vk::F4),
    (scancode::F5, vk::F5),
    (scancode::F6, vk::F6),
    (scancode::F7, vk::F7),
    (scancode::F8, vk::F8),
    (scancode::F9, vk::F9),
    (scancode::F10, vk::F10),
    (scancode::F11, vk::F11),
    (scancode::F12, vk::F12),
    (scancode::F13, vk::F13),
    (scancode::F14, vk::F14),
    (scancode::F15, vk::F15),
    (scancode::F16, vk::F16),
    (scancode::F17, vk::F17),
    (scancode::F18, vk::F18),
    (scancode::F19, vk::F19),
    (scancode::F20, vk::F20),
    (scancode::INSERT, vk::HELP), // macOS spells insert help
    (scancode::HOME, vk::HOME),
    (scancode::PAGEUP, vk::PAGE_UP),
    (scancode::DELETE, vk::FORWARD_DELETE),
    (scancode::END, vk::END),
    (scancode::PAGEDOWN, vk::PAGE_DOWN),
    (scancode::RIGHT, vk::RIGHT_ARROW),
    (scancode::LEFT, vk::LEFT_ARROW),
    (scancode::DOWN, vk::DOWN_ARROW),
    (scancode::UP, vk::UP_ARROW),
    (scancode::NUMLOCKCLEAR, vk::ANSI_KEYPAD_CLEAR),
    (scancode::KP_DIVIDE, vk::ANSI_KEYPAD_DIVIDE),
    (scancode::KP_MULTIPLY, vk::ANSI_KEYPAD_MULTIPLY),
    (scancode::KP_MINUS, vk::ANSI_KEYPAD_MINUS),
    (scancode::KP_PLUS, vk::ANSI_KEYPAD_PLUS),
    (scancode::KP_ENTER, vk::KEYPAD_ENTER),
    (scancode::KP_1, vk::ANSI_KEYPAD_1),
    (scancode::KP_2, vk::ANSI_KEYPAD_2),
    (scancode::KP_3, vk::ANSI_KEYPAD_3),
    (scancode::KP_4, vk::ANSI_KEYPAD_4),
    (scancode::KP_5, vk::ANSI_KEYPAD_5),
    (scancode::KP_6, vk::ANSI_KEYPAD_6),
    (scancode::KP_7, vk::ANSI_KEYPAD_7),
    (scancode::KP_8, vk::ANSI_KEYPAD_8),
    (scancode::KP_9, vk::ANSI_KEYPAD_9),
    (scancode::KP_0, vk::ANSI_KEYPAD_0),
    (scancode::KP_PERIOD, vk::ANSI_KEYPAD_DECIMAL),
    (scancode::KP_EQUALS, vk::ANSI_KEYPAD_EQUALS),
    (scancode::MUTE, vk::MUTE),
    (scancode::VOLUMEUP, vk::VOLUME_UP),
    (scancode::VOLUMEDOWN, vk::VOLUME_DOWN),
    // The JIS keys, whose scancodes SDL names by their USB numbers.
    (scancode::INTERNATIONAL1, vk::JIS_UNDERSCORE),
    (scancode::INTERNATIONAL3, vk::JIS_YEN),
    (scancode::LANG1, vk::JIS_KANA),
    (scancode::LANG2, vk::JIS_EISU),
    (scancode::LCTRL, vk::CONTROL),
    (scancode::LSHIFT, vk::SHIFT),
    (scancode::LALT, vk::OPTION),
    (scancode::LGUI, vk::COMMAND),
    (scancode::RCTRL, vk::RIGHT_CONTROL),
    (scancode::RSHIFT, vk::RIGHT_SHIFT),
    (scancode::RALT, vk::RIGHT_OPTION),
    (scancode::RGUI, vk::RIGHT_COMMAND),
];

/// The key code an event carries for a scancode nothing here names.
///
/// Not zero: zero is `A`, and a key that pressed `A` because this table did not
/// know it would be a bug with nothing to say so.
pub const UNKNOWN: u16 = 0xFFFF;

/// `ROSEMARY_COMMAND_KEY`: `<key>[+ctrl:<keys>]`. `<key>` is the host key the
/// guest sees as Command - `super` (the default), `ctrl` or `alt` - and Super
/// takes over the role it had, so the guest never loses a modifier. `+ctrl:`
/// lets Control stand in for Command in chords with `<keys>` only, which is
/// `edit` (A, C, V, X, Y and Z) or a run of letters and digits.
pub const COMMAND_KEY_ENV: &str = "ROSEMARY_COMMAND_KEY";

/// The published spelling of `ROSEMARY_COMMAND_KEY=ctrl`, still honoured when
/// the new variable is unset so that existing setups keep working.
const SWAP_CTRL_CMD_ENV: &str = "ROSEMARY_SWAP_CTRL_CMD";

/// `ROSEMARY_CONTROL_EDIT_KEYS=1`: shorthand for
/// `ROSEMARY_COMMAND_KEY=super+ctrl:edit`, when that is unset.
pub const CONTROL_EDIT_KEYS_ENV: &str = "ROSEMARY_CONTROL_EDIT_KEYS";

/// A host modifier key, both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKey {
    Ctrl,
    Alt,
    Super,
}

impl HostKey {
    const ALL: [HostKey; 3] = [HostKey::Ctrl, HostKey::Alt, HostKey::Super];

    /// The `SDL_Keymod` bits of both sides.
    fn bits(self) -> u16 {
        match self {
            HostKey::Ctrl => keymod::CTRL,
            HostKey::Alt => keymod::ALT,
            HostKey::Super => keymod::GUI,
        }
    }

    fn scancodes(self) -> [u32; 2] {
        match self {
            HostKey::Ctrl => [scancode::LCTRL, scancode::RCTRL],
            HostKey::Alt => [scancode::LALT, scancode::RALT],
            HostKey::Super => [scancode::LGUI, scancode::RGUI],
        }
    }
}

/// The parsed `ROSEMARY_COMMAND_KEY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandKey {
    /// The host key reported as Command.
    pub key: HostKey,
    /// One bit per scancode whose Control chord is reported as Command; zero
    /// for none. Every such scancode is below 64.
    pub control_keys: u64,
}

/// The `edit` preset's letters: select all, copy, paste, cut, redo and undo.
const EDIT_KEYS: &str = "acvxyz";

impl CommandKey {
    pub const DEFAULT: CommandKey = CommandKey {
        key: HostKey::Super,
        control_keys: 0,
    };

    /// `None` for a value this does not recognise.
    pub fn parse(value: &str) -> Option<CommandKey> {
        let value = value.trim().to_ascii_lowercase();
        let (key, stand_in) = match value.split_once('+') {
            Some((key, stand_in)) => (key.trim(), Some(stand_in.trim())),
            None => (value.as_str(), None),
        };
        let key = match key {
            "super" => HostKey::Super,
            "ctrl" => HostKey::Ctrl,
            "alt" => HostKey::Alt,
            _ => return None,
        };
        let control_keys = match stand_in {
            None => 0,
            // Control is Command already; standing it in again means nothing.
            Some(_) if key == HostKey::Ctrl => return None,
            Some(stand_in) => match stand_in.strip_prefix("ctrl:")?.trim() {
                "edit" => edit_mask(),
                keys => key_mask(keys)?,
            },
        };
        Some(CommandKey { key, control_keys })
    }

    /// The role `host` plays for the guest, named by the host key that plays
    /// it when nothing is remapped: Ctrl is Control, Alt Option, Super Command.
    fn role(self, host: HostKey) -> HostKey {
        match host {
            _ if host == self.key => HostKey::Super,
            HostKey::Super => self.key,
            _ => host,
        }
    }

    /// Host `SDL_Keymod` bits as the guest's: each modifier moved to the bits
    /// of the role it plays, side for side.
    fn remap(self, sdl_modifiers: u16) -> u16 {
        let all = keymod::CTRL | keymod::ALT | keymod::GUI;
        HostKey::ALL
            .into_iter()
            .fold(sdl_modifiers & !all, |out, host| {
                let held = (sdl_modifiers & host.bits()) >> host.bits().trailing_zeros();
                out | held << self.role(host).bits().trailing_zeros()
            })
    }

    /// A host modifier key's scancode as the guest's: the key of its role, on
    /// the same side.
    fn remap_scancode(self, code: u32) -> u32 {
        for host in HostKey::ALL {
            if let Some(side) = host.scancodes().iter().position(|s| *s == code) {
                return self.role(host).scancodes()[side];
            }
        }
        code
    }
}

/// One bit per scancode for a run of letters and digits; `None` for an empty
/// run or one with anything else in it.
fn key_mask(keys: &str) -> Option<u64> {
    if keys.is_empty() {
        return None;
    }
    keys.chars().try_fold(0u64, |mask, c| {
        let code = match c {
            'a'..='z' => scancode::A + (c as u32 - 'a' as u32),
            '1'..='9' => scancode::KEY_1 + (c as u32 - '1' as u32),
            '0' => scancode::KEY_0,
            _ => return None,
        };
        Some(mask | 1 << code)
    })
}

/// The configuration the variables ask for: `ROSEMARY_COMMAND_KEY`'s value,
/// else the swap, else the editing preset. An unrecognised value is the
/// default, with a warning.
fn command_key_from(value: Option<&str>, swap: bool, edit_keys: bool) -> CommandKey {
    if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
        return CommandKey::parse(value).unwrap_or_else(|| {
            eprintln!(
                "rosemary: appkit: {COMMAND_KEY_ENV}={value} is not recognised; using the default"
            );
            CommandKey::DEFAULT
        });
    }
    match swap {
        true => {
            eprintln!(
                "rosemary: appkit: {SWAP_CTRL_CMD_ENV} is deprecated; use {COMMAND_KEY_ENV}=ctrl"
            );
            CommandKey {
                key: HostKey::Ctrl,
                ..CommandKey::DEFAULT
            }
        }
        false => CommandKey {
            control_keys: if edit_keys { edit_mask() } else { 0 },
            ..CommandKey::DEFAULT
        },
    }
}

fn edit_mask() -> u64 {
    key_mask(EDIT_KEYS).unwrap_or_default()
}

pub fn command_key() -> CommandKey {
    static KEY: std::sync::OnceLock<CommandKey> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| {
        command_key_from(
            std::env::var(COMMAND_KEY_ENV).ok().as_deref(),
            std::env::var_os(SWAP_CTRL_CMD_ENV).is_some_and(|v| v != "0"),
            std::env::var_os(CONTROL_EDIT_KEYS_ENV).is_some_and(|v| v != "0"),
        )
    })
}

/// The modifiers the guest sees held, from the host's: the command key's
/// remap, then an editing chord's Control as Command. A chord never runs
/// with Control as the command key, so the remap leaves Control where it was.
fn guest_modifiers(sdl_modifiers: u16) -> u16 {
    let remapped = command_key().remap(sdl_modifiers);
    match edit_chord() {
        0 => remapped,
        _ => control_as_command(remapped),
    }
}

/// Each held Control reported as the Command on the same side.
fn control_as_command(sdl_modifiers: u16) -> u16 {
    let ctrl = sdl_modifiers & keymod::CTRL;
    (sdl_modifiers & !ctrl) | (ctrl << 4)
}

/// The Control sides (`SDL_Keymod` bits) being reported as Command, from the
/// key that started the chord until Control is let go; zero outside one. Held
/// across the chord rather than the one key, so that Control-A then Control-C
/// is one selection and copy and not two Command presses with a Control press
/// between them.
static EDIT_CHORD: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

pub fn edit_chord() -> u16 {
    EDIT_CHORD.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_edit_chord(sides: u16) {
    EDIT_CHORD.store(sides & keymod::CTRL, std::sync::atomic::Ordering::Relaxed);
}

/// A key from `keys` with Control and no other modifier but Shift.
fn is_edit_chord_key(keys: u64, scancode: u32, sdl_modifiers: u16) -> bool {
    scancode < 64
        && keys & 1 << scancode != 0
        && sdl_modifiers & keymod::CTRL != 0
        && sdl_modifiers & (keymod::ALT | keymod::GUI) == 0
}

/// What a key event does to the editing chord, decided before the event is
/// made so that its key code and flags follow the chord's new state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordStep {
    /// Nothing: the event goes out as the chord already has it.
    Keep,
    /// A chord key outside a chord: Control becomes Command first.
    Begin,
    /// A key that is not a chord key, pressed during one, or an event that
    /// shows no Control held: Command goes back to Control (or is let go)
    /// first, so every other Control chord is left alone.
    Leave,
    /// The last Control let go: the event is the release of Command, and the
    /// chord is over once it has gone out.
    EndAfter,
}

/// [`ChordStep`] for a key event, given the chord keys and the chord's
/// current sides. Pure, so the rules can be tested without the global state.
fn chord_step(keys: u64, chord: u16, scancode: u32, sdl_modifiers: u16, down: bool) -> ChordStep {
    let control_key = matches!(scancode, scancode::LCTRL | scancode::RCTRL);
    let control_held = sdl_modifiers & keymod::CTRL != 0;
    if chord == 0 {
        return match down && is_edit_chord_key(keys, scancode, sdl_modifiers) {
            true => ChordStep::Begin,
            false => ChordStep::Keep,
        };
    }
    match (control_key, control_held) {
        (true, false) if !down => ChordStep::EndAfter,
        (true, _) => ChordStep::Keep,
        // A Control release this pump never saw - dropped under a modal
        // dialog, say - must not leave the next Control press as Command.
        (false, false) => ChordStep::Leave,
        (false, true) if down && !is_modifier(scancode) => {
            match is_edit_chord_key(keys, scancode, sdl_modifiers) {
                true => ChordStep::Keep,
                false => ChordStep::Leave,
            }
        }
        (false, true) => ChordStep::Keep,
    }
}

/// [`chord_step`] for this key event, or [`ChordStep::Keep`] when
/// `ROSEMARY_COMMAND_KEY` names no Control chords.
pub fn edit_chord_step(scancode: u32, sdl_modifiers: u16, down: bool) -> ChordStep {
    match command_key().control_keys {
        0 => ChordStep::Keep,
        keys => chord_step(keys, edit_chord(), scancode, sdl_modifiers, down),
    }
}

/// The flags of a chord's `-flagsChanged:`: `rest` held as the host reports
/// it, with the Control sides `control` held as Control and `command` held as
/// Command, whatever the chord's state.
pub fn chord_flags(rest: u16, control: u16, command: u16) -> u64 {
    flags_of(command_key().remap(rest) | control | control_as_command(command))
}

/// The Command an editing chord reports in place of Control on `side`.
pub fn chord_command(side: u16) -> u16 {
    match side {
        keymod::RCTRL => vk::RIGHT_COMMAND,
        _ => vk::COMMAND,
    }
}

/// The Control key code for `side`, which the chord took over.
pub fn chord_control(side: u16) -> u16 {
    match side {
        keymod::RCTRL => vk::RIGHT_CONTROL,
        _ => vk::CONTROL,
    }
}

/// The scancode a virtual key code names, the inverse of [`virtual_key`]
/// before any remapping.
pub fn scancode_of(vk: u16) -> Option<u32> {
    VIRTUAL_KEYS
        .iter()
        .find(|(_, v)| *v == vk)
        .map(|(scancode, _)| *scancode)
}

pub fn virtual_key(scancode: u32) -> u16 {
    let scancode = guest_scancode(command_key(), edit_chord() != 0, scancode);
    VIRTUAL_KEYS
        .iter()
        .find(|(sdl, _)| *sdl == scancode)
        .map_or(UNKNOWN, |(_, vk)| *vk)
}

/// The scancode whose key code `scancode` is reported as. A chord moves only
/// Control: Super stays Command, as its flags do.
fn guest_scancode(key: CommandKey, chord: bool, scancode: u32) -> u32 {
    match (chord, scancode) {
        (true, scancode::LCTRL) => scancode::LGUI,
        (true, scancode::RCTRL) => scancode::RGUI,
        _ => key.remap_scancode(scancode),
    }
}

/// `NSEventModifierFlags`.
pub mod flags {
    pub const CAPS_LOCK: u64 = 1 << 16;
    pub const SHIFT: u64 = 1 << 17;
    pub const CONTROL: u64 = 1 << 18;
    pub const OPTION: u64 = 1 << 19;
    pub const COMMAND: u64 = 1 << 20;
    pub const NUMERIC_PAD: u64 = 1 << 21;
    pub const FUNCTION: u64 = 1 << 23;

    /// `NX_DEVICE*KEYMASK`: which side is held, in the low word AppKit leaves
    /// undocumented. Unreal's `-flagsChanged:` reads only these.
    pub const DEVICE_LCTL: u64 = 0x0001;
    pub const DEVICE_LSHIFT: u64 = 0x0002;
    pub const DEVICE_RSHIFT: u64 = 0x0004;
    pub const DEVICE_LCMD: u64 = 0x0008;
    pub const DEVICE_RCMD: u64 = 0x0010;
    pub const DEVICE_LALT: u64 = 0x0020;
    pub const DEVICE_RALT: u64 = 0x0040;
    pub const DEVICE_RCTL: u64 = 0x2000;
}

/// The guest's `NSEventModifierFlags` for the host's held modifiers.
pub fn modifiers(sdl_modifiers: u16) -> u64 {
    flags_of(guest_modifiers(sdl_modifiers))
}

/// `SDL_Keymod` bits already in the guest's terms, as AppKit flags.
fn flags_of(sdl_modifiers: u16) -> u64 {
    let mut out = 0;
    for (from, to) in [
        (keymod::SHIFT, flags::SHIFT),
        (keymod::CTRL, flags::CONTROL),
        (keymod::ALT, flags::OPTION),
        (keymod::GUI, flags::COMMAND),
        (keymod::CAPS, flags::CAPS_LOCK),
        (keymod::LCTRL, flags::DEVICE_LCTL),
        (keymod::RCTRL, flags::DEVICE_RCTL),
        (keymod::LSHIFT, flags::DEVICE_LSHIFT),
        (keymod::RSHIFT, flags::DEVICE_RSHIFT),
        (keymod::LALT, flags::DEVICE_LALT),
        (keymod::RALT, flags::DEVICE_RALT),
        (keymod::LGUI, flags::DEVICE_LCMD),
        (keymod::RGUI, flags::DEVICE_RCMD),
    ] {
        if sdl_modifiers & from != 0 {
            out |= to;
        }
    }
    out
}

/// The flags a key event carries for the key itself, on top of the held
/// modifiers. `NumericPad` is a property of the key, never of Num Lock: the
/// keypad keys have it, and so do the arrows, which also carry `Function`.
pub fn key_flags(scancode: u32) -> u64 {
    match scancode {
        scancode::UP | scancode::DOWN | scancode::LEFT | scancode::RIGHT => {
            flags::NUMERIC_PAD | flags::FUNCTION
        }
        scancode::NUMLOCKCLEAR
        | scancode::KP_DIVIDE
        | scancode::KP_MULTIPLY
        | scancode::KP_MINUS
        | scancode::KP_PLUS
        | scancode::KP_ENTER
        | scancode::KP_1
        | scancode::KP_2
        | scancode::KP_3
        | scancode::KP_4
        | scancode::KP_5
        | scancode::KP_6
        | scancode::KP_7
        | scancode::KP_8
        | scancode::KP_9
        | scancode::KP_0
        | scancode::KP_PERIOD
        | scancode::KP_EQUALS => flags::NUMERIC_PAD,
        _ => 0,
    }
}

/// Carbon's `EventModifiers` bits, which `GetCurrentKeyModifiers` answers in.
mod carbon {
    pub const CMD_KEY: u32 = 1 << 8;
    pub const SHIFT_KEY: u32 = 1 << 9;
    pub const ALPHA_LOCK: u32 = 1 << 10;
    pub const OPTION_KEY: u32 = 1 << 11;
    pub const CONTROL_KEY: u32 = 1 << 12;
}

/// `UInt32 GetCurrentKeyModifiers(void)`: the modifier keys held now, in
/// Carbon's bits. Qt's key mapper asks this rather than `+[NSEvent modifierFlags]`.]
#[allow(non_snake_case)]
pub unsafe extern "C" fn GetCurrentKeyModifiers() -> u32 {
    let held = guest_modifiers(sdl3::video::keyboard_modifiers());
    [
        (keymod::GUI, carbon::CMD_KEY),
        (keymod::SHIFT, carbon::SHIFT_KEY),
        (keymod::CAPS, carbon::ALPHA_LOCK),
        (keymod::ALT, carbon::OPTION_KEY),
        (keymod::CTRL, carbon::CONTROL_KEY),
    ]
    .into_iter()
    .filter(|(from, _)| held & from != 0)
    .fold(0, |out, (_, to)| out | to)
}

/// Whether a scancode is a modifier, which AppKit reports as `-flagsChanged:`
/// and never as a key.
pub fn is_modifier(scancode: u32) -> bool {
    (scancode::LCTRL..=scancode::RGUI).contains(&scancode) || scancode == scancode::CAPSLOCK
}

/// The keycode a key types with its shift-like modifiers applied.
///
/// SDL's event keycode is the unshifted key, but AppKit's `-characters` and
/// `-charactersIgnoringModifiers` both honor shift: Shift-/ is "?", not "/".
/// A guest that sees "/" there acts on a slash the user never typed.
pub fn typed_key(scancode: u32, keycode: u32, sdl_modifiers: u16) -> u32 {
    let shifting = sdl_modifiers & (keymod::SHIFT | keymod::CAPS | keymod::MODE);
    if shifting == 0 {
        return keycode;
    }
    sdl3::video::key_from_scancode(scancode, shifting).unwrap_or(keycode)
}

/// The text `-characters` reports, from SDL's keycode.
///
/// SDL's keycode for a printable key *is* its Unicode scalar, so those need no
/// table. The rest are the private-use scalars AppKit puts in `-characters` for
/// a function key, because a guest walking the string sees the same length and
/// the same values it would on Apple's - and a key with neither is the empty
/// string, which is what AppKit gives for a modifier.
pub fn characters(code: u32, keycode: u32) -> String {
    if let Some(c) = char::from_u32(keycode).filter(|c| !c.is_control()) {
        return c.to_string();
    }
    let private = match code {
        scancode::UP => 0xF700,
        scancode::DOWN => 0xF701,
        scancode::LEFT => 0xF702,
        scancode::RIGHT => 0xF703,
        scancode::HOME => 0xF729,
        scancode::END => 0xF72B,
        scancode::PAGEUP => 0xF72C,
        scancode::PAGEDOWN => 0xF72D,
        scancode::DELETE => 0xF728,
        scancode::INSERT => 0xF727,
        scancode::F1..=scancode::F12 => 0xF704 + (code - scancode::F1),
        scancode::RETURN | scancode::KP_ENTER => 0x000D,
        scancode::BACKSPACE => 0x0008,
        scancode::ESCAPE => 0x001B,
        scancode::TAB => 0x0009,
        _ => return String::new(),
    };
    char::from_u32(private)
        .map(String::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit() -> u64 {
        edit_mask()
    }

    #[test]
    fn an_editing_letter_with_control_begins_a_chord() {
        let ctrl = keymod::LCTRL;
        assert_eq!(
            chord_step(edit(), 0, scancode::V, ctrl, true),
            ChordStep::Begin
        );
        assert_eq!(
            chord_step(edit(), 0, scancode::Z, ctrl | keymod::LSHIFT, true),
            ChordStep::Begin
        );
        assert_eq!(
            chord_step(edit(), 0, scancode::V, ctrl, false),
            ChordStep::Keep
        );
        assert_eq!(
            chord_step(edit(), 0, scancode::S, ctrl, true),
            ChordStep::Keep
        );
        assert_eq!(chord_step(edit(), 0, scancode::V, 0, true), ChordStep::Keep);
        assert_eq!(
            chord_step(edit(), 0, scancode::V, ctrl | keymod::LALT, true),
            ChordStep::Keep
        );
        assert_eq!(
            chord_step(edit(), 0, scancode::V, ctrl | keymod::LGUI, true),
            ChordStep::Keep
        );
    }

    #[test]
    fn another_editing_letter_stays_in_the_chord() {
        let (chord, ctrl) = (keymod::LCTRL, keymod::LCTRL);
        assert_eq!(
            chord_step(edit(), chord, scancode::C, ctrl, true),
            ChordStep::Keep
        );
        assert_eq!(
            chord_step(edit(), chord, scancode::A, ctrl, false),
            ChordStep::Keep
        );
        assert_eq!(
            chord_step(edit(), chord, scancode::LSHIFT, ctrl | keymod::LSHIFT, true),
            ChordStep::Keep
        );
    }

    #[test]
    fn any_other_control_chord_leaves_the_edit_chord() {
        let (chord, ctrl) = (keymod::LCTRL, keymod::LCTRL);
        assert_eq!(
            chord_step(edit(), chord, scancode::S, ctrl, true),
            ChordStep::Leave
        );
        assert_eq!(
            chord_step(edit(), chord, scancode::V, ctrl | keymod::LALT, true),
            ChordStep::Leave
        );
    }

    #[test]
    fn the_chord_ends_only_when_the_last_control_is_let_go() {
        let chord = keymod::LCTRL | keymod::RCTRL;
        assert_eq!(
            chord_step(edit(), chord, scancode::LCTRL, keymod::RCTRL, false),
            ChordStep::Keep
        );
        assert_eq!(
            chord_step(edit(), chord, scancode::RCTRL, 0, false),
            ChordStep::EndAfter
        );
    }

    #[test]
    fn a_key_without_control_leaves_a_stale_chord() {
        assert_eq!(
            chord_step(edit(), keymod::LCTRL, scancode::W, 0, true),
            ChordStep::Leave
        );
        assert_eq!(
            chord_step(
                edit(),
                keymod::LCTRL,
                scancode::LSHIFT,
                keymod::LSHIFT,
                true
            ),
            ChordStep::Leave
        );
    }

    #[test]
    fn a_chord_reports_control_as_command_side_for_side() {
        assert_eq!(
            control_as_command(keymod::LCTRL | keymod::LSHIFT),
            keymod::LGUI | keymod::LSHIFT
        );
        assert_eq!(control_as_command(keymod::RCTRL), keymod::RGUI);
        assert_eq!(
            control_as_command(keymod::LCTRL | keymod::RCTRL),
            keymod::GUI
        );
    }

    #[test]
    fn a_chord_moves_control_and_leaves_super() {
        let key = CommandKey::DEFAULT;
        assert_eq!(guest_scancode(key, true, scancode::LCTRL), scancode::LGUI);
        assert_eq!(guest_scancode(key, true, scancode::RCTRL), scancode::RGUI);
        assert_eq!(guest_scancode(key, true, scancode::LGUI), scancode::LGUI);
        assert_eq!(guest_scancode(key, false, scancode::LCTRL), scancode::LCTRL);
    }

    fn command(key: HostKey) -> CommandKey {
        CommandKey {
            key,
            ..CommandKey::DEFAULT
        }
    }

    #[test]
    fn super_is_command_by_default() {
        assert_eq!(command_key_from(None, false, false), CommandKey::DEFAULT);
        assert_eq!(
            command_key_from(Some(" "), false, false),
            CommandKey::DEFAULT
        );
        assert_eq!(CommandKey::parse("super"), Some(CommandKey::DEFAULT));
        let key = CommandKey::DEFAULT;
        assert_eq!(
            key.remap(keymod::LCTRL | keymod::RGUI),
            keymod::LCTRL | keymod::RGUI
        );
        assert_eq!(key.remap_scancode(scancode::LGUI), scancode::LGUI);
    }

    #[test]
    fn ctrl_as_command_hands_super_the_control_role() {
        let key = CommandKey::parse("Ctrl").unwrap();
        assert_eq!(key, command(HostKey::Ctrl));
        assert_eq!(
            key.remap(keymod::LCTRL | keymod::LSHIFT),
            keymod::LGUI | keymod::LSHIFT
        );
        assert_eq!(key.remap(keymod::RGUI), keymod::RCTRL);
        assert_eq!(key.remap(keymod::LALT), keymod::LALT);
        assert_eq!(key.remap_scancode(scancode::RCTRL), scancode::RGUI);
        assert_eq!(key.remap_scancode(scancode::LGUI), scancode::LCTRL);
        assert_eq!(key.remap_scancode(scancode::A), scancode::A);
    }

    #[test]
    fn alt_as_command_hands_super_the_option_role() {
        let key = CommandKey::parse("alt").unwrap();
        assert_eq!(key.remap(keymod::RALT), keymod::RGUI);
        assert_eq!(key.remap(keymod::LGUI), keymod::LALT);
        assert_eq!(key.remap(keymod::LCTRL), keymod::LCTRL);
        assert_eq!(key.remap_scancode(scancode::LALT), scancode::LGUI);
        assert_eq!(key.remap_scancode(scancode::RGUI), scancode::RALT);
    }

    #[test]
    fn the_swap_variable_still_means_ctrl() {
        assert_eq!(command_key_from(None, true, false), command(HostKey::Ctrl));
        assert_eq!(
            command_key_from(Some("alt"), true, false),
            command(HostKey::Alt)
        );
    }

    #[test]
    fn control_chords_take_the_edit_preset_or_named_keys() {
        let edit = CommandKey::parse("super+ctrl:edit").unwrap();
        assert_eq!(edit.key, HostKey::Super);
        assert_eq!(edit.control_keys, key_mask("acvxyz").unwrap());
        let named = CommandKey::parse(" alt + ctrl:S1 ").unwrap();
        assert_eq!(named.key, HostKey::Alt);
        assert_eq!(named.control_keys, 1 << scancode::S | 1 << scancode::KEY_1);
        assert!(is_edit_chord_key(
            named.control_keys,
            scancode::KEY_1,
            keymod::LCTRL
        ));
        assert!(!is_edit_chord_key(
            named.control_keys,
            scancode::V,
            keymod::LCTRL
        ));
    }

    #[test]
    fn the_edit_keys_variable_is_the_edit_preset() {
        let preset = CommandKey::parse("super+ctrl:edit").unwrap();
        assert_eq!(command_key_from(None, false, true), preset);
        assert_eq!(
            command_key_from(Some("alt"), false, true),
            command(HostKey::Alt)
        );
        // As before, the full swap wins over the narrow one.
        assert_eq!(command_key_from(None, true, true), command(HostKey::Ctrl));
    }

    #[test]
    fn a_malformed_command_key_is_rejected() {
        for value in [
            "meta",
            "ctrl+ctrl:edit",
            "super+alt:edit",
            "super+ctrl:",
            "super+ctrl:s-v",
            "super+edit",
        ] {
            assert_eq!(CommandKey::parse(value), None, "{value}");
        }
        assert_eq!(
            command_key_from(Some("meta"), true, true),
            CommandKey::DEFAULT
        );
    }

    /// Two keys with the same virtual code would be one key as far as a guest's
    /// lookup table is concerned.
    #[test]
    fn no_two_scancodes_share_a_virtual_key() {
        let mut seen: Vec<u16> = VIRTUAL_KEYS.iter().map(|(_, vk)| *vk).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "a virtual key code appears twice");
    }

    /// Apple's own numbers, by name, as the anchor for the rest.
    #[test]
    fn the_documented_codes_are_the_documented_codes() {
        assert_eq!(virtual_key(scancode::A), 0x00, "kVK_ANSI_A");
        assert_eq!(virtual_key(scancode::Z), 0x06, "kVK_ANSI_Z");
        assert_eq!(virtual_key(scancode::SPACE), 0x31, "kVK_Space");
        assert_eq!(virtual_key(scancode::UP), 0x7E, "kVK_UpArrow");
        // The keypad's, whose ADB order skips 0x5A - and 0x5A is F20, which is
        // the pair a transcription slip puts on top of each other.
        assert_eq!(virtual_key(scancode::KP_7), 0x59, "kVK_ANSI_Keypad7");
        assert_eq!(virtual_key(scancode::KP_8), 0x5B, "kVK_ANSI_Keypad8");
        assert_eq!(virtual_key(scancode::F20), 0x5A, "kVK_F20");
        // The function keys are not consecutive in ADB order either.
        assert_eq!(virtual_key(scancode::F1), 0x7A, "kVK_F1");
        assert_eq!(virtual_key(scancode::F3), 0x63, "kVK_F3");
        assert_eq!(virtual_key(scancode::F13), 0x69, "kVK_F13");
    }

    #[test]
    fn a_scancode_with_no_macos_code_is_not_the_a_key() {
        assert_eq!(virtual_key(scancode::UNKNOWN), UNKNOWN);
        assert_eq!(virtual_key(400), UNKNOWN);
    }

    #[test]
    fn characters_are_the_scalar_for_text_and_apples_private_use_for_the_rest() {
        assert_eq!(characters(scancode::A, 'a' as u32), "a");
        assert_eq!(characters(scancode::UP, 0x40000052), "\u{F700}", "up arrow");
        assert_eq!(
            characters(scancode::LSHIFT, 0x400000E1),
            "",
            "a modifier types nothing"
        );
    }

    #[test]
    fn sdl_modifiers_become_appkit_flags() {
        assert_eq!(
            modifiers(keymod::RSHIFT),
            flags::SHIFT | flags::DEVICE_RSHIFT
        );
        assert_eq!(
            modifiers(keymod::LCTRL | keymod::LALT),
            flags::CONTROL | flags::OPTION | flags::DEVICE_LCTL | flags::DEVICE_LALT
        );
        assert_eq!(modifiers(keymod::NONE), 0);
    }

    #[test]
    fn num_lock_is_not_the_numeric_keypad_modifier() {
        assert_eq!(modifiers(keymod::NUM), 0);
    }

    #[test]
    fn keypad_and_arrow_keys_carry_the_numeric_pad_flag() {
        assert_eq!(key_flags(scancode::KP_5), flags::NUMERIC_PAD);
        assert_eq!(key_flags(scancode::KP_ENTER), flags::NUMERIC_PAD);
        assert_eq!(
            key_flags(scancode::UP),
            flags::NUMERIC_PAD | flags::FUNCTION
        );
        assert_eq!(key_flags(scancode::A), 0, "a letter with Num Lock held");
    }

    #[test]
    fn the_modifier_keys_are_the_modifiers() {
        assert!(is_modifier(scancode::LSHIFT));
        assert!(is_modifier(scancode::RGUI));
        assert!(is_modifier(scancode::CAPSLOCK));
        assert!(!is_modifier(scancode::A));
        assert!(!is_modifier(scancode::MODE));
    }
}

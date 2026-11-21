use std::ptr;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSEventModifierFlags};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUserDefaults, ns_string};
use crate::localization as loc;

use Code::*;

// Carbon virtual key codes, the same physical keys registered by global-hotkey.
const MAC_KEYS: &[(u16, Code)] = &[
    (0, KeyA), (1, KeyS), (2, KeyD), (3, KeyF), (4, KeyH), (5, KeyG),
    (6, KeyZ), (7, KeyX), (8, KeyC), (9, KeyV), (11, KeyB), (12, KeyQ),
    (13, KeyW), (14, KeyE), (15, KeyR), (16, KeyY), (17, KeyT),
    (18, Digit1), (19, Digit2), (20, Digit3), (21, Digit4), (22, Digit6), (23, Digit5),
    (24, Equal), (25, Digit9), (26, Digit7), (27, Minus), (28, Digit8), (29, Digit0),
    (30, BracketRight), (31, KeyO), (32, KeyU), (33, BracketLeft), (34, KeyI), (35, KeyP),
    (36, Enter), (37, KeyL), (38, KeyJ), (39, Quote), (40, KeyK), (41, Semicolon),
    (42, Backslash), (43, Comma), (44, Slash), (45, KeyN), (46, KeyM), (47, Period),
    (48, Tab), (49, Space), (50, Backquote), (51, Backspace),
    (64, F17), (79, F18), (80, F19), (90, F20),
    (96, F5), (97, F6), (98, F7), (99, F3), (100, F8), (101, F9), (103, F11),
    (105, F13), (106, F16), (107, F14), (109, F10), (111, F12), (113, F15),
    (115, Home), (116, PageUp), (117, Delete), (118, F4), (119, End), (120, F2),
    (121, PageDown), (122, F1), (123, ArrowLeft), (124, ArrowRight), (125, ArrowDown), (126, ArrowUp),
];

pub(crate) fn default_hotkey() -> HotKey {
    HotKey::new(Some(Modifiers::META | Modifiers::SHIFT), KeyX)
}

pub(crate) fn configured_hotkey() -> HotKey {
    NSUserDefaults::standardUserDefaults()
        .stringForKey(ns_string!("CaptureShortcut"))
        .and_then(|value| value.to_string().parse::<HotKey>().ok())
        .filter(|key| validate(*key).is_ok())
        .unwrap_or_else(default_hotkey)
}

pub(crate) fn save_hotkey(hotkey: HotKey) {
    let value = NSString::from_str(&hotkey.to_string());
    // NSUserDefaults accepts property list strings.
    unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&value), ns_string!("CaptureShortcut")); }
}

pub(crate) fn from_event(event: &NSEvent) -> Result<HotKey, &'static str> {
    let code = MAC_KEYS.iter().find(|(key, _)| *key == event.keyCode())
        .map(|(_, code)| *code).ok_or("This key cannot be used for capture. Try another one.")?;
    let flags = event.modifierFlags();
    let mut modifiers = Modifiers::empty();
    for (flag, modifier) in [
        (NSEventModifierFlags::Control, Modifiers::CONTROL),
        (NSEventModifierFlags::Option, Modifiers::ALT),
        (NSEventModifierFlags::Shift, Modifiers::SHIFT),
        (NSEventModifierFlags::Command, Modifiers::META),
    ] {
        if flags.contains(flag) {
            modifiers |= modifier;
        }
    }
    let hotkey = HotKey::new(Some(modifiers), code);
    validate(hotkey)?;
    Ok(hotkey)
}

fn validate(hotkey: HotKey) -> Result<(), &'static str> {
    let main_mods = Modifiers::CONTROL | Modifiers::ALT | Modifiers::META;
    if !hotkey.mods.intersects(main_mods) || hotkey.mods == Modifiers::META {
        return Err("Use Control, Option, or Command with another modifier.");
    }
    if !(hotkey.mods - (main_mods | Modifiers::SHIFT)).is_empty()
        || !MAC_KEYS.iter().any(|(_, code)| *code == hotkey.key)
    {
        return Err("This combination cannot be used for capture. Try another one.");
    }
    if (hotkey.mods == Modifiers::META | Modifiers::SHIFT && matches!(hotkey.key, KeyZ | KeyV | KeyS))
        || (hotkey.mods == Modifiers::META | Modifiers::ALT && matches!(hotkey.key, KeyV | Escape))
    {
        return Err("This combination is used for a standard command. Try another one.");
    }
    Ok(())
}

pub(crate) fn label(hotkey: HotKey) -> String {
    let mut label = String::new();
    for (modifier, symbol) in [
        (Modifiers::CONTROL, '⌃'), (Modifiers::ALT, '⌥'),
        (Modifiers::SHIFT, '⇧'), (Modifiers::META, '⌘'),
    ] {
        if hotkey.mods.contains(modifier) { label.push(symbol); }
    }
    let name = match hotkey.key {
        Space => "Space", Backspace => "⌫", Delete => "⌦", Enter => "↩", Tab => "⇥",
        ArrowLeft => "←", ArrowRight => "→", ArrowUp => "↑", ArrowDown => "↓",
        Equal => "=", Minus => "-", BracketLeft => "[", BracketRight => "]",
        Quote => "'", Semicolon => ";", Backslash => "\\", Comma => ",", Slash => "/",
        Period => ".", Backquote => "`",
        _ => {
            let name = hotkey.key.to_string();
            label.push_str(name.strip_prefix("Key").or_else(|| name.strip_prefix("Digit")).unwrap_or(&name));
            return label;
        }
    };
    label.push_str(&loc::string(name));
    label
}

type SymbolicHotkeys = NSArray<NSDictionary<NSString, NSNumber>>;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn CopySymbolicHotKeys(result: *mut *mut SymbolicHotkeys) -> i32;
}

pub(crate) fn check_system_shortcut(hotkey: HotKey) -> Result<(), &'static str> {
    validate(hotkey)?;
    let key = MAC_KEYS.iter().find(|(_, code)| *code == hotkey.key)
        .map(|(key, _)| u32::from(*key)).ok_or("This key is unavailable.")?;
    let mut flags = 0;
    for (modifier, flag) in [
        (Modifiers::META, 1 << 8), (Modifiers::SHIFT, 1 << 9),
        (Modifiers::ALT, 1 << 11), (Modifiers::CONTROL, 1 << 12),
    ] {
        if hotkey.mods.contains(modifier) { flags |= flag; }
    }
    // Carbon returns an owned CFArray of CFDictionaries. These bridge to NSArray
    // and NSDictionary, and their CFNumber/CFBoolean values to NSNumber.
    let mut entries = ptr::null_mut();
    if unsafe { CopySymbolicHotKeys(&mut entries) } != 0 {
        return Err("Could not check system shortcuts. Try again.");
    }
    let entries = unsafe { Retained::from_raw(entries) }.ok_or("Could not read system shortcuts. Try again.")?;
    for entry in entries.iter() {
        let active = entry.objectForKey(ns_string!("kHISymbolicHotKeyEnabled")).is_some_and(|value| value.boolValue());
        let code = entry.objectForKey(ns_string!("kHISymbolicHotKeyCode")).map(|value| value.unsignedIntValue());
        let mods = entry.objectForKey(ns_string!("kHISymbolicHotKeyModifiers")).map(|value| value.unsignedIntValue());
        if active && code == Some(key) && mods == Some(flags) {
            return Err("macOS is using this shortcut. Try a different one.");
        }
    }
    Ok(())
}

//! Small helpers shared by the views.

use std::future::Future;

use anyhow::{Result, anyhow};

/// Run a fallible future on the I/O runtime and await it from the UI thread.
pub async fn io<T, F>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    match dbclaw_drivers::spawn(future).await {
        Ok(result) => result,
        Err(e) if e.is_cancelled() => Err(anyhow!("cancelled")),
        Err(e) => Err(anyhow!("background task failed: {e}")),
    }
}

/// `12345` → `12,345`
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn format_duration(d: std::time::Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1.0 {
        format!("{:.2} ms", ms)
    } else if ms < 1000.0 {
        format!("{:.0} ms", ms)
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

/// Make a value safe to draw on one line: cut long text, show newlines as ⏎.
pub fn single_line(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars + 4));
    for (n, ch) in s.chars().enumerate() {
        if n >= max_chars {
            out.push('…');
            break;
        }
        match ch {
            '\n' => out.push('⏎'),
            '\r' => {}
            '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A keyboard shortcut for display in the platform's style, written like the key
/// bindings: `kbd("secondary-shift-enter")` is `⇧⌘↵` on macOS and `Ctrl+Shift+Enter`
/// on Windows and Linux (`secondary` = ⌘ on macOS, Ctrl elsewhere).
pub fn kbd(keys: &str) -> String {
    kbd_for(keys, cfg!(target_os = "macos"))
}

fn kbd_for(keys: &str, mac: bool) -> String {
    let (mods, key) = match keys.rsplit_once('-') {
        // A trailing "-" is the minus key itself ("ctrl--").
        Some((m, "")) => (m.trim_end_matches('-'), "-"),
        Some((m, k)) => (m, k),
        None => ("", keys),
    };
    let has = |m: &str| mods.split('-').any(|x| x == m);
    let (ctrl, alt, shift) = (has("ctrl"), has("alt"), has("shift"));
    let cmd = has("cmd") || (mac && has("secondary"));
    let ctrl = ctrl || (!mac && has("secondary"));
    let key = match (key, mac) {
        ("enter", true) => "↵".to_string(),
        ("backspace", true) => "⌫".to_string(),
        ("tab", true) => "⇥".to_string(),
        ("escape", _) => "Esc".to_string(),
        ("enter", false) => "Enter".to_string(),
        ("backspace", false) => "Backspace".to_string(),
        ("tab", false) => "Tab".to_string(),
        ("space", _) => "Space".to_string(),
        (k, _) => k.to_uppercase(),
    };
    if mac {
        // macOS order: ⌃⌥⇧⌘ then the key, no separators.
        let mut out = String::new();
        for (on, sym) in [(ctrl, "⌃"), (alt, "⌥"), (shift, "⇧"), (cmd, "⌘")] {
            if on {
                out.push_str(sym);
            }
        }
        out + &key
    } else {
        let mut parts: Vec<String> = Vec::new();
        for (on, name) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (cmd, "Win")] {
            if on {
                parts.push(name.to_string());
            }
        }
        parts.push(key);
        parts.join("+")
    }
}

/// Move the item at `from` to position `to` (drag and drop reordering): it ends up
/// after the item at `to` when moving right, before it when moving left.
pub fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from == to || from >= items.len() || to >= items.len() {
        return;
    }
    let item = items.remove(from);
    items.insert(to, item);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }

    #[test]
    fn shortcuts_per_platform() {
        assert_eq!(kbd_for("secondary-t", true), "⌘T");
        assert_eq!(kbd_for("secondary-t", false), "Ctrl+T");
        assert_eq!(kbd_for("secondary-shift-enter", true), "⇧⌘↵");
        assert_eq!(kbd_for("secondary-shift-enter", false), "Ctrl+Shift+Enter");
        assert_eq!(kbd_for("secondary-backspace", true), "⌘⌫");
        assert_eq!(kbd_for("secondary-backspace", false), "Ctrl+Backspace");
        assert_eq!(kbd_for("ctrl-space", true), "⌃Space");
        assert_eq!(kbd_for("ctrl-space", false), "Ctrl+Space");
        assert_eq!(kbd_for("secondary-]", false), "Ctrl+]");
        assert_eq!(kbd_for("secondary-.", true), "⌘.");
        assert_eq!(kbd_for("f2", false), "F2");
    }

    #[test]
    fn move_items() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        move_item(&mut v, 0, 2); // right: lands after 'c'
        assert_eq!(v, ['b', 'c', 'a', 'd']);
        move_item(&mut v, 3, 0); // left: lands before 'b'
        assert_eq!(v, ['d', 'b', 'c', 'a']);
        move_item(&mut v, 1, 1);
        move_item(&mut v, 9, 0);
        assert_eq!(v, ['d', 'b', 'c', 'a']);
    }

    #[test]
    fn one_line() {
        assert_eq!(single_line("a\nb", 10), "a⏎b");
        assert_eq!(single_line("abcdef", 3), "abc…");
    }
}

//! Read/write a launched instance's Minecraft keybinds.
//!
//! Keybinds are not declared anywhere static — every mod registers its
//! `KeyMapping`s in code at runtime, and Minecraft writes the full set into
//! `options.txt` in the game directory (which Anvil points at the instance
//! dir). So this module only ever parses/edits `<instance>/options.txt`; an
//! instance never launched has no such file and reports `launched: false`.
//!
//! Keybind lines look like `key_<name>:<token>`, e.g.
//! `key_key.attack:key.mouse.left`, `key_key.jei.toggleOverlay:key.keyboard.o`.
//! Every other line is an unrelated setting and is preserved verbatim.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::instance::instance_dir;

/// Vanilla keybind action names (the segment right after `key.`). Anything
/// else's first segment is treated as a mod id.
const VANILLA: &[&str] = &[
    "attack",
    "use",
    "forward",
    "back",
    "left",
    "right",
    "jump",
    "sneak",
    "sprint",
    "drop",
    "inventory",
    "chat",
    "playerlist",
    "pickItem",
    "command",
    "socialInteractions",
    "screenshot",
    "togglePerspective",
    "smoothCamera",
    "fullscreen",
    "spectatorOutlines",
    "swapOffhand",
    "hotbar",
    "saveToolbarActivator",
    "loadToolbarActivator",
    "advancements",
];

#[derive(Debug, Clone, Serialize)]
pub struct Keybind {
    /// The raw `options.txt` name, e.g. "key.jei.toggleOverlay".
    pub name: String,
    /// Human label, e.g. "Toggle Overlay".
    pub label: String,
    /// Raw token, e.g. "key.keyboard.o" / "key.keyboard.unknown".
    pub key_token: String,
    /// Pretty token, e.g. "O" / "Mouse Left" / "Unbound".
    pub key_display: String,
    /// True if this bind shares its (non-unbound) key with another bind.
    pub conflict: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyGroup {
    pub mod_name: String,
    pub binds: Vec<Keybind>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeybindReport {
    /// False when the instance has no options.txt (never launched).
    pub launched: bool,
    pub groups: Vec<KeyGroup>,
    /// Number of distinct keys bound by more than one action.
    pub conflict_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeyChange {
    pub name: String,
    pub token: String,
}

const UNBOUND: &str = "key.keyboard.unknown";

fn options_path(instance_id: &str) -> PathBuf {
    instance_dir(instance_id).join("options.txt")
}

/// Split on separators AND camelCase boundaries, then Title Case each word:
/// "toggleOverlay" -> "Toggle Overlay", "hotbar.1" -> "Hotbar 1".
fn humanize(s: &str) -> String {
    let mut spaced = String::with_capacity(s.len() + 4);
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if c == '.' || c == '_' || c == '-' || c == '/' {
            spaced.push(' ');
        } else {
            if let Some(p) = prev {
                if c.is_ascii_uppercase() && (p.is_ascii_lowercase() || p.is_ascii_digit()) {
                    spaced.push(' ');
                }
            }
            spaced.push(c);
        }
        prev = Some(c);
    }
    spaced
        .split_whitespace()
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// (group, label) for a keybind name. Vanilla actions group under "Minecraft";
/// everything else under its Title-Cased mod id.
fn group_and_label(name: &str) -> (String, String) {
    let rest = name.strip_prefix("key.").unwrap_or(name);
    let first = rest.split('.').next().unwrap_or(rest);
    if VANILLA.contains(&first) {
        ("Minecraft".to_string(), humanize(rest))
    } else {
        let label_src = rest
            .strip_prefix(first)
            .and_then(|s| s.strip_prefix('.'))
            .filter(|s| !s.is_empty())
            .unwrap_or(rest);
        (humanize(first), humanize(label_src))
    }
}

fn pretty_token(tok: &str) -> String {
    if tok == UNBOUND {
        return "Unbound".to_string();
    }
    if let Some(k) = tok.strip_prefix("key.keyboard.") {
        humanize(k)
    } else if let Some(m) = tok.strip_prefix("key.mouse.") {
        format!("Mouse {}", humanize(m))
    } else {
        tok.to_string()
    }
}

/// Parse `key_<name>:<token>` lines out of options.txt content.
fn parse_lines(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|l| l.strip_prefix("key_"))
        .filter_map(|rest| {
            rest.split_once(':')
                .map(|(n, t)| (n.to_string(), t.to_string()))
        })
        .collect()
}

pub fn read_keybinds(instance_id: &str) -> KeybindReport {
    let Ok(content) = std::fs::read_to_string(options_path(instance_id)) else {
        return KeybindReport {
            launched: false,
            groups: Vec::new(),
            conflict_count: 0,
        };
    };

    let binds = parse_lines(&content);

    // token -> how many binds use it (ignoring the unbound sentinel).
    let mut uses: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, tok) in &binds {
        if tok != UNBOUND {
            *uses.entry(tok.as_str()).or_insert(0) += 1;
        }
    }
    let conflict_count = uses.values().filter(|&&n| n >= 2).count();

    let mut groups: BTreeMap<String, Vec<Keybind>> = BTreeMap::new();
    for (name, token) in &binds {
        let (group, label) = group_and_label(name);
        let conflict =
            token != UNBOUND && uses.get(token.as_str()).copied().unwrap_or(0) >= 2;
        groups.entry(group).or_default().push(Keybind {
            name: name.clone(),
            label,
            key_token: token.clone(),
            key_display: pretty_token(token),
            conflict,
        });
    }

    // Minecraft first, then mods alphabetically; binds by label.
    let mut out: Vec<KeyGroup> = groups
        .into_iter()
        .map(|(mod_name, mut binds)| {
            binds.sort_by(|a, b| a.label.cmp(&b.label));
            KeyGroup { mod_name, binds }
        })
        .collect();
    out.sort_by(|a, b| match (a.mod_name == "Minecraft", b.mod_name == "Minecraft") {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.mod_name.cmp(&b.mod_name),
    });

    KeybindReport {
        launched: true,
        groups: out,
        conflict_count,
    }
}

/// Rewrite only the targeted `key_<name>:` lines; every other line (and the
/// file's exact ordering / trailing-newline shape) is preserved byte-for-byte.
pub fn write_keybinds(
    instance_id: &str,
    changes: &[KeyChange],
) -> std::io::Result<()> {
    let path = options_path(instance_id);
    let content = std::fs::read_to_string(&path)?;
    let map: std::collections::HashMap<&str, &str> = changes
        .iter()
        .map(|c| (c.name.as_str(), c.token.as_str()))
        .collect();

    let mut out = String::with_capacity(content.len() + 16);
    for line in content.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        let new_body = body
            .strip_prefix("key_")
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(name, _)| map.get(name).map(|t| format!("key_{name}:{t}")))
            .unwrap_or_else(|| body.to_string());
        out.push_str(&new_body);
        out.push_str(nl);
    }
    std::fs::write(&path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "version:3465\n\
key_key.attack:key.mouse.left\n\
key_key.jump:key.keyboard.space\n\
key_key.sneak:key.keyboard.left.shift\n\
key_key.jei.toggleOverlay:key.keyboard.o\n\
key_key.create.toolbelt:key.keyboard.left.shift\n\
key_key.jade.config:key.keyboard.unknown\n\
fov:0.5";

    #[test]
    fn groups_and_labels() {
        let binds = parse_lines(SAMPLE);
        assert_eq!(binds.len(), 6); // 6 key_ lines, not the version/fov lines

        assert_eq!(
            group_and_label("key.attack"),
            ("Minecraft".to_string(), "Attack".to_string())
        );
        assert_eq!(
            group_and_label("key.jei.toggleOverlay"),
            ("Jei".to_string(), "Toggle Overlay".to_string())
        );
        assert_eq!(
            group_and_label("key.create.toolbelt"),
            ("Create".to_string(), "Toolbelt".to_string())
        );
        assert_eq!(pretty_token("key.keyboard.left.shift"), "Left Shift");
        assert_eq!(pretty_token("key.mouse.left"), "Mouse Left");
        assert_eq!(pretty_token(UNBOUND), "Unbound");
    }

    #[test]
    fn conflict_detection() {
        // sneak and create.toolbelt both bind left.shift -> conflict.
        // jade.config is unbound -> never a conflict.
        let mut uses: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, t) in parse_lines(SAMPLE) {
            if t != UNBOUND {
                *uses.entry(Box::leak(t.into_boxed_str())).or_insert(0) += 1;
            }
        }
        assert_eq!(uses.get("key.keyboard.left.shift"), Some(&2));
        assert_eq!(uses.values().filter(|&&n| n >= 2).count(), 1);
    }

    #[test]
    fn write_only_touches_targeted_keys() {
        let changes = vec![KeyChange {
            name: "key.jei.toggleOverlay".to_string(),
            token: "key.keyboard.r".to_string(),
        }];
        let map: std::collections::HashMap<&str, &str> = changes
            .iter()
            .map(|c| (c.name.as_str(), c.token.as_str()))
            .collect();
        let mut out = String::new();
        for line in SAMPLE.split_inclusive('\n') {
            let (body, nl) = match line.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (line, ""),
            };
            let nb = body
                .strip_prefix("key_")
                .and_then(|r| r.split_once(':'))
                .and_then(|(n, _)| map.get(n).map(|t| format!("key_{n}:{t}")))
                .unwrap_or_else(|| body.to_string());
            out.push_str(&nb);
            out.push_str(nl);
        }
        assert!(out.contains("key_key.jei.toggleOverlay:key.keyboard.r"));
        // Untouched lines preserved exactly, including non-key lines.
        assert!(out.contains("version:3465\n"));
        assert!(out.contains("key_key.attack:key.mouse.left\n"));
        assert!(out.ends_with("fov:0.5"));
        // Same number of lines, nothing dropped or invented.
        assert_eq!(out.lines().count(), SAMPLE.lines().count());
    }
}

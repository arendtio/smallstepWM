//! Keyboard layout for the seat.
//!
//! An empty xkbcommon layout follows `XKB_DEFAULT_LAYOUT`. When that variable is unset,
//! libxkbcommon falls back to `us`. SDDM does not hand the console keymap to a Wayland
//! session, so the compositor reads the system keyboard configuration itself.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLayout {
    pub layout: String,
    pub variant: String,
    pub model: String,
    pub options: Option<String>,
    pub source: &'static str,
}

pub fn load() -> ResolvedLayout {
    let env_layout = std::env::var("XKB_DEFAULT_LAYOUT").ok().filter(|value| !value.is_empty());
    let env_variant = std::env::var("XKB_DEFAULT_VARIANT").unwrap_or_default();
    let env_model = std::env::var("XKB_DEFAULT_MODEL").unwrap_or_default();
    let env_options = std::env::var("XKB_DEFAULT_OPTIONS").ok().filter(|value| !value.is_empty());
    let xorg = std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf").unwrap_or_default();
    let vconsole = std::fs::read_to_string("/etc/vconsole.conf").unwrap_or_default();
    resolve(
        env_layout.as_deref(),
        &env_variant,
        &env_model,
        env_options.as_deref(),
        &xorg,
        &vconsole,
    )
}

pub fn resolve(
    env_layout: Option<&str>,
    env_variant: &str,
    env_model: &str,
    env_options: Option<&str>,
    xorg: &str,
    vconsole: &str,
) -> ResolvedLayout {
    if let Some(layout) = env_layout {
        return ResolvedLayout {
            layout: layout.to_string(),
            variant: env_variant.to_string(),
            model: env_model.to_string(),
            options: env_options.map(str::to_string),
            source: "environment",
        };
    }
    if let Some(layout) = xorg_option(xorg, "XkbLayout").filter(|value| !value.is_empty()) {
        return ResolvedLayout {
            layout,
            variant: xorg_option(xorg, "XkbVariant").unwrap_or_default(),
            model: xorg_option(xorg, "XkbModel").unwrap_or_default(),
            options: xorg_option(xorg, "XkbOptions").filter(|value| !value.is_empty()),
            source: "xorg",
        };
    }
    if let Some(keymap) = vconsole_keymap(vconsole) {
        let (layout, variant) = console_keymap_to_xkb(keymap);
        return ResolvedLayout {
            layout,
            variant,
            model: String::new(),
            options: None,
            source: "vconsole",
        };
    }
    ResolvedLayout {
        layout: "us".to_string(),
        variant: String::new(),
        model: String::new(),
        options: None,
        source: "fallback",
    }
}

fn xorg_option(text: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\"");
    for line in text.lines() {
        let Some(pos) = line.find(&marker) else {
            continue;
        };
        let after = &line[pos + marker.len()..];
        let start = after.find('"')?;
        let rest = &after[start + 1..];
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    None
}

fn vconsole_keymap(text: &str) -> Option<&str> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("KEYMAP=") else {
            continue;
        };
        let keymap = rest.trim().trim_matches('"');
        if !keymap.is_empty() {
            return Some(keymap);
        }
    }
    None
}

fn console_keymap_to_xkb(keymap: &str) -> (String, String) {
    if let Some(layout) = keymap.strip_suffix("-latin1-nodeadkeys") {
        return (layout.to_string(), "nodeadkeys".to_string());
    }
    if let Some(layout) = keymap.strip_suffix("-latin1") {
        return (layout.to_string(), String::new());
    }
    if keymap == "de" || keymap == "de_DE" {
        return ("de".to_string(), String::new());
    }
    (keymap.to_string(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xorg_layout_wins_when_the_environment_is_empty() {
        let text = r#"
Section "InputClass"
    Identifier "system-keyboard"
    Option "XkbLayout" "de"
    Option "XkbVariant" "nodeadkeys"
    Option "XkbModel" "pc105"
EndSection
"#;
        let resolved = resolve(None, "", "", None, text, "KEYMAP=us");
        assert_eq!(resolved.layout, "de");
        assert_eq!(resolved.variant, "nodeadkeys");
        assert_eq!(resolved.model, "pc105");
        assert_eq!(resolved.source, "xorg");
    }

    #[test]
    fn console_keymap_maps_german_latin1() {
        let resolved = resolve(None, "", "", None, "", "KEYMAP=de-latin1\n");
        assert_eq!(resolved.layout, "de");
        assert_eq!(resolved.variant, "");
        assert_eq!(resolved.source, "vconsole");
    }

    #[test]
    fn environment_layout_is_kept() {
        let resolved = resolve(Some("de"), "nodeadkeys", "", None, "Option \"XkbLayout\" \"us\"", "");
        assert_eq!(resolved.layout, "de");
        assert_eq!(resolved.variant, "nodeadkeys");
        assert_eq!(resolved.source, "environment");
    }
}

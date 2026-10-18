//! Wi-Fi through whatever is already installed.
//!
//! NetworkManager (`nmcli`) is used when it answers. Otherwise iwd (`iwctl`).
//! The shell does not install a network stack. Work runs on a side thread so a scan
//! does not freeze the frame loop.

use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub signal: u8,
    pub secured: bool,
    pub connected: bool,
}

#[derive(Clone, Debug)]
pub enum WifiJob {
    Refresh { generation: u64 },
    Radio { generation: u64, on: bool },
    Connect {
        generation: u64,
        ssid: String,
        password: Option<String>,
    },
    Disconnect { generation: u64 },
}

#[derive(Clone, Debug)]
pub struct Panel {
    pub open: bool,
    pub busy: bool,
    pub generation: u64,
    pub radio_on: Option<bool>,
    pub device: Option<String>,
    pub networks: Vec<Network>,
    pub scroll: i32,
    pub scroll_carry: f64,
    pub note: String,
    pub password_for: Option<String>,
    pub password: String,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            open: false,
            busy: false,
            generation: 0,
            radio_on: None,
            device: None,
            networks: Vec::new(),
            scroll: 0,
            scroll_carry: 0.0,
            note: String::new(),
            password_for: None,
            password: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiReport {
    pub generation: u64,
    pub radio_on: Option<bool>,
    pub device: Option<String>,
    pub networks: Vec<Network>,
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stack {
    NetworkManager,
    Iwd,
}

pub fn perform(job: WifiJob) -> WifiReport {
    let generation = match &job {
        WifiJob::Refresh { generation }
        | WifiJob::Radio { generation, .. }
        | WifiJob::Connect { generation, .. }
        | WifiJob::Disconnect { generation } => *generation,
    };
    let Some(stack) = detect() else {
        return WifiReport {
            generation,
            radio_on: None,
            device: None,
            networks: Vec::new(),
            note: "No NetworkManager or iwd".into(),
        };
    };
    let failure = match &job {
        WifiJob::Radio { on, .. } => set_radio(stack, *on).err(),
        WifiJob::Connect { ssid, password, .. } => connect(stack, ssid, password.as_deref()).err(),
        WifiJob::Disconnect { .. } => disconnect(stack).err(),
        WifiJob::Refresh { .. } => None,
    };
    let mut report = refresh(stack, generation);
    if let Some(err) = failure {
        report.note = err;
    }
    report
}

fn detect() -> Option<Stack> {
    if succeeds("nmcli", &["-t", "-f", "STATE", "general"]) {
        Some(Stack::NetworkManager)
    } else if succeeds("iwctl", &["device", "list"]) {
        Some(Stack::Iwd)
    } else {
        None
    }
}

fn succeeds(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn captured(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd).args(args).output().map_err(|err| err.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().find(|line| !line.trim().is_empty()).unwrap_or("command failed");
        Err(trim_note(line))
    }
}

fn refresh(stack: Stack, generation: u64) -> WifiReport {
    match stack {
        Stack::NetworkManager => refresh_nm(generation),
        Stack::Iwd => refresh_iwd(generation),
    }
}

fn refresh_nm(generation: u64) -> WifiReport {
    let radio_on = captured("nmcli", &["-t", "-f", "WIFI", "radio"])
        .ok()
        .map(|text| text.trim() == "enabled");
    let device = captured("nmcli", &["-t", "-f", "DEVICE,TYPE", "device"])
        .ok()
        .as_deref()
        .and_then(nm_wifi_device);
    let listed = captured(
        "nmcli",
        &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", "yes"],
    )
    .or_else(|_| {
        captured(
            "nmcli",
            &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list"],
        )
    });
    match listed {
        Ok(text) => finish(generation, radio_on, device, parse_nmcli(&text)),
        Err(err) => WifiReport {
            generation,
            radio_on,
            device,
            networks: Vec::new(),
            note: err,
        },
    }
}

fn refresh_iwd(generation: u64) -> WifiReport {
    let devices = captured("iwctl", &["device", "list"])
        .ok()
        .map(|text| parse_iwctl_devices(&text))
        .unwrap_or_default();
    let Some((device, radio_on)) = devices.into_iter().next() else {
        return WifiReport {
            generation,
            radio_on: None,
            device: None,
            networks: Vec::new(),
            note: "No Wi-Fi device".into(),
        };
    };
    if radio_on {
        let _ = captured("iwctl", &["station", &device, "scan"]);
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }
    match captured("iwctl", &["station", &device, "get-networks"]) {
        Ok(text) => finish(generation, Some(radio_on), Some(device), parse_iwctl_networks(&text)),
        Err(err) => WifiReport {
            generation,
            radio_on: Some(radio_on),
            device: Some(device),
            networks: Vec::new(),
            note: err,
        },
    }
}

fn finish(generation: u64, radio_on: Option<bool>, device: Option<String>, networks: Vec<Network>) -> WifiReport {
    let note = if device.is_none() {
        "No Wi-Fi device".into()
    } else if radio_on == Some(false) {
        "Wi-Fi is off".into()
    } else if let Some(connected) = networks.iter().find(|net| net.connected) {
        connected.ssid.clone()
    } else {
        format!("{} networks", networks.len())
    };
    WifiReport {
        generation,
        radio_on,
        device,
        networks,
        note,
    }
}

fn set_radio(stack: Stack, on: bool) -> Result<(), String> {
    match stack {
        Stack::NetworkManager => {
            let verb = if on { "on" } else { "off" };
            captured("nmcli", &["radio", "wifi", verb]).map(|_| ())
        }
        Stack::Iwd => {
            let device = iwctl_device()?.0;
            let verb = if on { "on" } else { "off" };
            captured("iwctl", &["device", &device, "set-property", "Powered", verb]).map(|_| ())
        }
    }
}

fn connect(stack: Stack, ssid: &str, password: Option<&str>) -> Result<(), String> {
    match stack {
        Stack::NetworkManager => {
            let mut args = vec!["-w", "25", "device", "wifi", "connect", ssid];
            if let Some(password) = password {
                args.push("password");
                args.push(password);
            }
            run_redacted("nmcli", &args, password)
        }
        Stack::Iwd => {
            let device = iwctl_device()?.0;
            let mut args = Vec::new();
            if let Some(password) = password {
                args.extend(["--passphrase", password]);
            }
            args.extend(["station", &device, "connect", ssid]);
            run_redacted("iwctl", &args, password)
        }
    }
}

fn disconnect(stack: Stack) -> Result<(), String> {
    match stack {
        Stack::NetworkManager => {
            let device = nm_wifi_device(&captured("nmcli", &["-t", "-f", "DEVICE,TYPE", "device"])?)
                .ok_or_else(|| "No Wi-Fi device".to_string())?;
            captured("nmcli", &["device", "disconnect", &device]).map(|_| ())
        }
        Stack::Iwd => {
            let device = iwctl_device()?.0;
            captured("iwctl", &["station", &device, "disconnect"]).map(|_| ())
        }
    }
}

fn iwctl_device() -> Result<(String, bool), String> {
    let text = captured("iwctl", &["device", "list"])?;
    parse_iwctl_devices(&text)
        .into_iter()
        .next()
        .ok_or_else(|| "No Wi-Fi device".to_string())
}

fn run_redacted(cmd: &str, args: &[&str], secret: Option<&str>) -> Result<(), String> {
    let out = Command::new(cmd).args(args).output().map_err(|err| err.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err.lines().find(|line| !line.trim().is_empty()).unwrap_or("command failed");
    let mut note = trim_note(line);
    if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
        note = note.replace(secret, "•••");
    }
    Err(note)
}

fn trim_note(text: &str) -> String {
    text.trim().chars().take(90).collect()
}

pub fn parse_nmcli(text: &str) -> Vec<Network> {
    let mut networks = Vec::new();
    for line in text.lines() {
        let fields = split_nmcli(line.trim());
        if fields.len() < 3 {
            continue;
        }
        let ssid = fields[1].trim().to_string();
        if ssid.is_empty() {
            continue;
        }
        let signal = fields[2].trim().parse::<u8>().unwrap_or(0).min(100);
        let security = fields.get(3).map(|value| value.trim()).unwrap_or("");
        let secured = !security.is_empty() && security != "--" && security != "open";
        networks.push(Network {
            ssid,
            signal,
            secured,
            connected: fields[0].trim() == "*",
        });
    }
    dedup(networks)
}

fn split_nmcli(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
        } else if ch == ':' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    fields.push(current);
    fields
}

pub fn parse_iwctl_devices(text: &str) -> Vec<(String, bool)> {
    let mut devices = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(powered) = parts.last().copied() else {
            continue;
        };
        if powered != "on" && powered != "off" {
            continue;
        }
        let name = parts[0];
        if name == "Name" || name.starts_with('-') {
            continue;
        }
        devices.push((name.to_string(), powered == "on"));
    }
    devices
}

pub fn parse_iwctl_networks(text: &str) -> Vec<Network> {
    let mut networks = Vec::new();
    for line in text.lines() {
        let connected = line.trim_start().starts_with('>');
        let line = line.trim().trim_start_matches('>').trim();
        let mut parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let Some(signal) = signal_token(parts.pop().unwrap()) else {
            continue;
        };
        let security = parts.pop().unwrap();
        let ssid = parts.join(" ");
        if ssid.is_empty() || ssid == "Network" {
            continue;
        }
        let secured = !matches!(security, "open" | "--");
        networks.push(Network {
            ssid,
            signal,
            secured,
            connected,
        });
    }
    dedup(networks)
}

fn signal_token(token: &str) -> Option<u8> {
    if !token.is_empty() && token.chars().all(|ch| ch == '*') && token.len() <= 4 {
        return Some((token.len() as u8 * 25).min(100));
    }
    let dbm = token.parse::<i32>().ok()?;
    Some(((dbm + 100).clamp(0, 100)) as u8)
}

fn nm_wifi_device(text: &str) -> Option<String> {
    for line in text.lines() {
        let fields = split_nmcli(line.trim());
        if fields.get(1).map(|kind| kind == "wifi").unwrap_or(false) {
            if !fields[0].is_empty() {
                return Some(fields[0].clone());
            }
        }
    }
    None
}

fn dedup(mut networks: Vec<Network>) -> Vec<Network> {
    networks.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)));
    let mut seen = std::collections::HashSet::new();
    networks.retain(|net| seen.insert(net.ssid.clone()));
    networks.truncate(48);
    networks
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordEdit {
    Insert(String),
    Backspace,
    Submit,
    Cancel,
    Ignore,
}

pub fn pop_char(text: &mut String) {
    if let Some((index, _)) = text.char_indices().next_back() {
        text.truncate(index);
    }
}

pub fn password_edit(sym: u32, text: &str) -> PasswordEdit {
    use smithay::input::keyboard::keysyms;
    if sym == keysyms::KEY_Escape {
        PasswordEdit::Cancel
    } else if sym == keysyms::KEY_Return || sym == keysyms::KEY_KP_Enter {
        PasswordEdit::Submit
    } else if sym == keysyms::KEY_BackSpace {
        PasswordEdit::Backspace
    } else if text.chars().any(|ch| ch.is_control()) || text.is_empty() {
        PasswordEdit::Ignore
    } else {
        PasswordEdit::Insert(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nmcli_unescapes_colons_and_marks_the_active_network() {
        let text = "*:Home\\:net:80:WPA2\n :Cafe:40:\n :hidden:10:WPA2\n";
        let nets = parse_nmcli(text);
        assert_eq!(nets[0].ssid, "Home:net");
        assert!(nets[0].connected && nets[0].secured && nets[0].signal == 80);
        assert_eq!(nets[1].ssid, "Cafe");
        assert!(!nets[1].secured);
        assert!(nets.iter().all(|net| net.ssid != "hidden" || net.secured));
    }

    #[test]
    fn iwctl_table_keeps_spaces_and_the_connected_mark() {
        let text = "\
                                 Network name                    Security  Signal
                                 -----------------------------------------------
                                 My Network                      psk       ****
                               > Cafe Guest                      open      **
                                 No networks available
";
        let nets = parse_iwctl_networks(text);
        assert_eq!(nets.len(), 2);
        assert_eq!(nets[0].ssid, "Cafe Guest");
        assert!(nets[0].connected);
        assert!(!nets[0].secured);
        assert_eq!(nets[0].signal, 50);
        assert!(nets[1].secured);
        assert_eq!(nets[1].signal, 100);
    }

    #[test]
    fn iwctl_device_line_reads_power() {
        let text = "\
                                    Name      Address            Powered
                                    ------------------------------------
                                    wlan0     aa:bb:cc:dd:ee:ff  off
";
        assert_eq!(parse_iwctl_devices(text), vec![("wlan0".into(), false)]);
    }

    #[test]
    fn password_keys() {
        use smithay::input::keyboard::keysyms;
        assert_eq!(password_edit(keysyms::KEY_Escape, ""), PasswordEdit::Cancel);
        assert_eq!(password_edit(keysyms::KEY_Return, "\r"), PasswordEdit::Submit);
        assert_eq!(password_edit(keysyms::KEY_BackSpace, ""), PasswordEdit::Backspace);
        assert_eq!(password_edit(keysyms::KEY_a, "ä"), PasswordEdit::Insert("ä".into()));
        assert_eq!(password_edit(0, ""), PasswordEdit::Ignore);
        let mut secret = "aä".to_string();
        pop_char(&mut secret);
        assert_eq!(secret, "a");
        pop_char(&mut secret);
        pop_char(&mut secret);
        assert!(secret.is_empty());
    }
}

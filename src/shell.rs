//! Start view and the left strip. No browser and no image decoder.

pub const STRIP_COLLAPSED: i32 = 48;
pub const STRIP_EXPANDED: i32 = 220;
const ROW: i32 = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Start,
    Browser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Start,
    Browser,
    Network,
    Power,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Logout,
    Shutdown,
    Reboot,
}

pub fn strip_width(expanded: bool) -> i32 {
    if expanded {
        STRIP_EXPANDED
    } else {
        STRIP_COLLAPSED
    }
}

/// The resting strip stays on screen, so windows begin at its right edge.
pub fn content_origin() -> (i32, i32) {
    (STRIP_COLLAPSED, 0)
}

pub fn content_size(output_w: i32, output_h: i32) -> (i32, i32) {
    ((output_w - STRIP_COLLAPSED).max(1), output_h.max(1))
}

pub const BAR_W: i32 = 180;
pub const BAR_H: i32 = 32;
pub const MENU_W: i32 = 168;
pub const MENU_H: i32 = 136;
const BTN_H: i32 = 40;
const BTN_GAP: i32 = 8;

pub const WIFI_W: i32 = 300;
pub const WIFI_HEADER: i32 = 36;
pub const WIFI_STATUS: i32 = 22;
pub const WIFI_ROW: i32 = 30;
pub const WIFI_VISIBLE: i32 = 6;
pub const WIFI_PASS: i32 = 36;
pub const WIFI_H: i32 = WIFI_HEADER + WIFI_STATUS + WIFI_VISIBLE * WIFI_ROW + WIFI_PASS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiHit {
    Toggle,
    Scan,
    Row(usize),
    Disconnect(usize),
    Join,
}

pub struct WifiPaint<'a> {
    pub name: &'a str,
    pub signal: u8,
    pub secured: bool,
    pub connected: bool,
}

/// Firefox does not report startup progress, so the mark loops instead of filling.
pub fn startup_phase(elapsed_ms: u128) -> f32 {
    (elapsed_ms % 1_400) as f32 / 1_400.0
}

pub fn hit_strip(expanded: bool, height: i32, x: f64, y: f64) -> Option<Entry> {
    if x < 0.0 || y < 0.0 || x >= f64::from(strip_width(expanded)) || y >= f64::from(height.max(0)) {
        return None;
    }
    if y >= f64::from(height - ROW) {
        return Some(Entry::Power);
    }
    if y >= f64::from(height - 2 * ROW) {
        return Some(Entry::Network);
    }
    let row = (y / f64::from(ROW)) as i32;
    match row {
        0 => Some(Entry::Start),
        1 => Some(Entry::Browser),
        _ => None,
    }
}

/// Sits just to the right of the expanded strip, beside the power icon.
pub fn power_menu_origin(output_h: i32) -> (i32, i32) {
    (STRIP_EXPANDED + 12, (output_h - 16 - MENU_H).max(0))
}

pub fn hit_power_action(output_h: i32, x: f64, y: f64) -> Option<PowerAction> {
    let (origin_x, origin_y) = power_menu_origin(output_h);
    let local_x = x - f64::from(origin_x);
    let local_y = y - f64::from(origin_y);
    if local_x < 0.0 || local_y < 0.0 || local_x >= f64::from(MENU_W) || local_y >= f64::from(MENU_H) {
        return None;
    }
    let stride = f64::from(BTN_H + BTN_GAP);
    let row = (local_y / stride) as i32;
    let within = local_y - f64::from(row) * stride;
    if within < 0.0 || within >= f64::from(BTN_H) {
        return None;
    }
    match row {
        0 => Some(PowerAction::Logout),
        1 => Some(PowerAction::Shutdown),
        2 => Some(PowerAction::Reboot),
        _ => None,
    }
}

/// XRGB8888, little-endian memory order B, G, R, X.
pub fn fill_gradient(width: i32, height: i32) -> Vec<u8> {
    let mut buf = vec![0u8; (width * height * 4) as usize];
    paint_gradient(&mut buf, width, height);
    buf
}

pub fn paint_gradient(buf: &mut [u8], width: i32, height: i32) {
    let top = [28u8, 36, 48];
    let bottom = [62u8, 72, 58];
    let last = (height - 1).max(1);
    for y in 0..height {
        let t = y as u32;
        let r = blend(top[0], bottom[0], t, last as u32);
        let g = blend(top[1], bottom[1], t, last as u32);
        let b = blend(top[2], bottom[2], t, last as u32);
        for x in 0..width {
            put(buf, width, x, y, r, g, b);
        }
    }
}

pub fn paint_strip(width: i32, height: i32, expanded: bool, page: Page, network_open: bool, power_open: bool) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            put(&mut pixels, width, x, y, 22, 24, 29);
        }
    }
    for (index, entry) in [Entry::Start, Entry::Browser].into_iter().enumerate() {
        let row_y = index as i32 * ROW;
        if row_y + ROW > height - 2 * ROW {
            break;
        }
        let active = match (page, entry) {
            (Page::Start, Entry::Start) | (Page::Browser, Entry::Browser) => true,
            _ => false,
        };
        paint_row(&mut pixels, width, height, row_y, expanded, active, entry);
    }
    if height >= 2 * ROW {
        paint_row(
            &mut pixels,
            width,
            height,
            height - 2 * ROW,
            expanded,
            network_open,
            Entry::Network,
        );
    }
    if height >= ROW {
        paint_row(&mut pixels, width, height, height - ROW, expanded, power_open, Entry::Power);
    }
    pixels
}

fn paint_row(buf: &mut [u8], width: i32, height: i32, row_y: i32, expanded: bool, active: bool, entry: Entry) {
    if active {
        fill_rect(buf, width, height, 0, row_y, 3, ROW, 215, 196, 163);
    }
    let icon = match entry {
        Entry::Start => image(include_bytes!("../assets/start.bgra")),
        Entry::Browser => image(include_bytes!("../assets/browser.bgra")),
        Entry::Network => image(include_bytes!("../assets/wifi.bgra")),
        Entry::Power => image(include_bytes!("../assets/power.bgra")),
    };
    blit(buf, width, height, 8, row_y + 8, &icon);
    if expanded {
        let label = match entry {
            Entry::Start => "Start",
            Entry::Browser => "Browser",
            Entry::Network => "Wifi",
            Entry::Power => "Power",
        };
        draw_text(buf, width, height, 48, row_y + 20, label);
    }
}

fn draw_text(buf: &mut [u8], width: i32, height: i32, x: i32, y: i32, text: &str) {
    let mut cursor = x;
    for ch in text.chars() {
        let glyph = glyph(ch);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    put(buf, width, cursor + col, y + row as i32, 230, 226, 216);
                }
            }
        }
        cursor += 6;
        if cursor >= width || y >= height {
            break;
        }
    }
}

fn glyph(ch: char) -> [u8; 7] {
    match ch {
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010, 0b01010],
        'f' => [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'd' => [0b00001, 0b00001, 0b01111, 0b10001, 0b10001, 0b10001, 0b01111],
        'h' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b10001],
        'u' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10001, 0b01111],
        'g' => [0b00000, 0b00000, 0b01110, 0b10000, 0b10011, 0b10001, 0b01110],
        'i' => [0b00100, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'n' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'a' => [0b00000, 0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111],
        'b' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110],
        'e' => [0b00000, 0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110],
        'o' => [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'r' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0b00000, 0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        't' => [0b00100, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110],
        'w' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010],
        _ => [0; 7],
    }
}

fn fill_rect(
    buf: &mut [u8],
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: u8,
    g: u8,
    b: u8,
) {
    for py in y..(y + h).min(height) {
        if py < 0 {
            continue;
        }
        for px in x..(x + w).min(width) {
            if px >= 0 {
                put(buf, width, px, py, r, g, b);
            }
        }
    }
}

fn put(buf: &mut [u8], width: i32, x: i32, y: i32, r: u8, g: u8, b: u8) {
    if x < 0 || y < 0 || x >= width {
        return;
    }
    let i = ((y * width + x) * 4) as usize;
    if i + 3 >= buf.len() {
        return;
    }
    buf[i] = b;
    buf[i + 1] = g;
    buf[i + 2] = r;
    buf[i + 3] = 255;
}

pub struct CursorImage {
    pub pixels: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub hot_x: i32,
    pub hot_y: i32,
}

pub fn paint_cursor() -> CursorImage {
    let cursor = image(include_bytes!("../assets/cursor.bgra"));
    CursorImage {
        pixels: cursor.pixels.to_vec(),
        width: cursor.width,
        height: cursor.height,
        hot_x: cursor.hot_x,
        hot_y: cursor.hot_y,
    }
}

pub fn paint_startup_bar(phase: f32) -> Vec<u8> {
    let phase = phase.clamp(0.0, 1.0);
    let mut pixels = vec![0u8; (BAR_W * BAR_H * 4) as usize];
    fill_rect(&mut pixels, BAR_W, BAR_H, 0, 0, BAR_W, BAR_H, 22, 24, 29);
    let label = "Starting";
    let text_w = label.len() as i32 * 6;
    draw_text(&mut pixels, BAR_W, BAR_H, (BAR_W - text_w) / 2, 4, label);
    let track_y = 20;
    let track_w = BAR_W - 16;
    fill_rect(&mut pixels, BAR_W, BAR_H, 8, track_y, track_w, 6, 12, 13, 16);
    let segment = track_w / 3;
    let travel = (track_w - segment).max(0);
    let x = 8 + (travel as f32 * phase).round() as i32;
    fill_rect(&mut pixels, BAR_W, BAR_H, x, track_y, segment, 6, 215, 196, 163);
    pixels
}

pub fn paint_power_menu() -> Vec<u8> {
    let mut pixels = vec![0u8; (MENU_W * MENU_H * 4) as usize];
    fill_rect(&mut pixels, MENU_W, MENU_H, 0, 0, MENU_W, MENU_H, 22, 24, 29);
    let actions = [
        (PowerAction::Logout, "Log out", include_bytes!("../assets/logout.bgra") as &[u8]),
        (PowerAction::Shutdown, "Shut down", include_bytes!("../assets/shutdown.bgra")),
        (PowerAction::Reboot, "Restart", include_bytes!("../assets/reboot.bgra")),
    ];
    for (index, (_, label, bytes)) in actions.into_iter().enumerate() {
        let y = index as i32 * (BTN_H + BTN_GAP);
        fill_rect(&mut pixels, MENU_W, MENU_H, 0, y, MENU_W, BTN_H, 42, 45, 54);
        let icon = image(bytes);
        blit(&mut pixels, MENU_W, MENU_H, 10, y + (BTN_H - icon.height) / 2, &icon);
        draw_text(&mut pixels, MENU_W, MENU_H, 42, y + 16, label);
    }
    pixels
}

struct Image {
    width: i32,
    height: i32,
    hot_x: i32,
    hot_y: i32,
    pixels: &'static [u8],
}

fn image(bytes: &'static [u8]) -> Image {
    let width = i32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let height = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let hot_x = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let hot_y = i32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let pixels = &bytes[16..];
    Image {
        width,
        height,
        hot_x,
        hot_y,
        pixels,
    }
}

fn blit(buf: &mut [u8], width: i32, height: i32, x: i32, y: i32, src: &Image) {
    for sy in 0..src.height {
        for sx in 0..src.width {
            let si = ((sy * src.width + sx) * 4) as usize;
            let alpha = src.pixels[si + 3];
            if alpha == 0 {
                continue;
            }
            let dx = x + sx;
            let dy = y + sy;
            if dx < 0 || dy < 0 || dx >= width || dy >= height {
                continue;
            }
            let di = ((dy * width + dx) * 4) as usize;
            if di + 3 >= buf.len() {
                continue;
            }
            if alpha == 255 {
                buf[di..di + 4].copy_from_slice(&src.pixels[si..si + 4]);
                continue;
            }
            let inv = u16::from(255 - alpha);
            let cover = u16::from(alpha);
            for channel in 0..3 {
                let mixed = u16::from(src.pixels[si + channel]) * cover + u16::from(buf[di + channel]) * inv;
                buf[di + channel] = (mixed / 255) as u8;
            }
            buf[di + 3] = 255;
        }
    }
}

fn blend(from: u8, to: u8, y: u32, last: u32) -> u8 {
    let v = u32::from(from) * (last - y) + u32::from(to) * y;
    (v / last) as u8
}

pub fn wifi_origin(output_h: i32) -> (i32, i32) {
    (STRIP_EXPANDED + 12, (output_h - ROW - 8 - WIFI_H).max(0))
}

pub fn clamp_wifi_scroll(scroll: i32, count: usize) -> i32 {
    let max = (count as i32 - WIFI_VISIBLE).max(0);
    scroll.clamp(0, max)
}

pub fn wifi_panel_contains(output_h: i32, x: f64, y: f64) -> bool {
    let (origin_x, origin_y) = wifi_origin(output_h);
    x >= f64::from(origin_x)
        && y >= f64::from(origin_y)
        && x < f64::from(origin_x + WIFI_W)
        && y < f64::from(origin_y + WIFI_H)
}

pub fn power_menu_contains(output_h: i32, x: f64, y: f64) -> bool {
    let (origin_x, origin_y) = power_menu_origin(output_h);
    x >= f64::from(origin_x)
        && y >= f64::from(origin_y)
        && x < f64::from(origin_x + MENU_W)
        && y < f64::from(origin_y + MENU_H)
}

pub fn hit_wifi(output_h: i32, x: f64, y: f64, count: usize, scroll: i32) -> Option<WifiHit> {
    if !wifi_panel_contains(output_h, x, y) {
        return None;
    }
    let (origin_x, origin_y) = wifi_origin(output_h);
    let local_x = x - f64::from(origin_x);
    let local_y = y - f64::from(origin_y);
    if local_y < f64::from(WIFI_HEADER) {
        if local_x >= f64::from(WIFI_W - 64) {
            return Some(WifiHit::Toggle);
        }
        if local_x >= 70.0 {
            return Some(WifiHit::Scan);
        }
        return None;
    }
    if local_y >= f64::from(WIFI_H - WIFI_PASS) {
        if local_x >= f64::from(WIFI_W - 64) {
            return Some(WifiHit::Join);
        }
        return None;
    }
    let list_y = local_y - f64::from(WIFI_HEADER + WIFI_STATUS);
    if list_y < 0.0 {
        return None;
    }
    let row = (list_y / f64::from(WIFI_ROW)) as usize;
    if row >= WIFI_VISIBLE as usize {
        return None;
    }
    let index = scroll.max(0) as usize + row;
    if index >= count {
        return None;
    }
    if local_x >= f64::from(WIFI_W - 36) {
        Some(WifiHit::Disconnect(index))
    } else {
        Some(WifiHit::Row(index))
    }
}

pub fn paint_wifi_panel(rows: &[WifiPaint<'_>], scroll: i32, radio_on: Option<bool>, status: &str, password: Option<&str>) -> Vec<u8> {
    let mut pixels = vec![0u8; (WIFI_W * WIFI_H * 4) as usize];
    fill_rect(&mut pixels, WIFI_W, WIFI_H, 0, 0, WIFI_W, WIFI_H, 22, 24, 29);
    draw_text8(&mut pixels, WIFI_W, 8, 12, "Wi-Fi", 230, 226, 216, WIFI_W);
    draw_text8(&mut pixels, WIFI_W, 78, 12, "Scan", 215, 196, 163, WIFI_W);
    let radio = match radio_on {
        Some(true) => "On",
        Some(false) => "Off",
        None => "--",
    };
    draw_text8(&mut pixels, WIFI_W, WIFI_W - 40, 12, radio, 230, 226, 216, WIFI_W);
    draw_text8(&mut pixels, WIFI_W, 8, WIFI_HEADER + 4, status, 180, 176, 166, WIFI_W);
    let scroll = scroll.max(0) as usize;
    for slot in 0..WIFI_VISIBLE as usize {
        let Some(row) = rows.get(scroll + slot) else {
            break;
        };
        let y = WIFI_HEADER + WIFI_STATUS + slot as i32 * WIFI_ROW;
        if row.connected {
            fill_rect(&mut pixels, WIFI_W, WIFI_H, 0, y, 3, WIFI_ROW, 215, 196, 163);
        }
        signal_bars(&mut pixels, 8, y + 8, row.signal);
        let mut name = row.name.to_string();
        if row.secured {
            name.insert_str(0, "* ");
        }
        draw_text8(&mut pixels, WIFI_W, 32, y + 10, &name, 230, 226, 216, WIFI_W - 36);
        if row.connected {
            draw_text8(&mut pixels, WIFI_W, WIFI_W - 28, y + 10, "x", 215, 196, 163, WIFI_W);
        }
    }
    let pass_y = WIFI_H - WIFI_PASS;
    fill_rect(&mut pixels, WIFI_W, WIFI_H, 0, pass_y, WIFI_W, WIFI_PASS, 42, 45, 54);
    if let Some(secret) = password {
        if secret.is_empty() {
            draw_text8(&mut pixels, WIFI_W, 8, pass_y + 12, "Password", 140, 136, 128, WIFI_W - 64);
        } else {
            let mut shown = String::new();
            for _ in secret.chars().take(24) {
                shown.push('•');
            }
            draw_text8(&mut pixels, WIFI_W, 8, pass_y + 12, &shown, 230, 226, 216, WIFI_W - 64);
        }
        draw_text8(&mut pixels, WIFI_W, WIFI_W - 48, pass_y + 12, "Join", 215, 196, 163, WIFI_W);
    }
    pixels
}

fn signal_bars(buf: &mut [u8], x: i32, y: i32, signal: u8) {
    let level = (u16::from(signal) * 4 / 100).min(4) as i32;
    for index in 0..4 {
        let height = 4 + index * 3;
        let (r, g, b) = if index < level { (215, 196, 163) } else { (50, 54, 62) };
        fill_rect(buf, WIFI_W, WIFI_H, x + index * 5, y + 14 - height, 3, height, r, g, b);
    }
}

fn draw_text8(buf: &mut [u8], width: i32, x: i32, y: i32, text: &str, r: u8, g: u8, b: u8, limit: i32) {
    let mut cursor = x;
    for ch in text.chars() {
        if cursor + 8 > limit {
            break;
        }
        if let Some(rows) = glyph8(ch) {
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..8 {
                    if bits & (1 << col) != 0 {
                        put(buf, width, cursor + col, y + row as i32, r, g, b);
                    }
                }
            }
        }
        cursor += 9;
    }
}

fn glyph8(ch: char) -> Option<[u8; 8]> {
    let code = ch as u32;
    if code < 128 {
        return Some(crate::font8x8::BASIC[code as usize]);
    }
    if (0xA0..=0xFF).contains(&code) {
        return Some(crate::font8x8::LATIN[(code - 0xA0) as usize]);
    }
    if ch == '•' {
        return Some([0x00, 0x18, 0x3C, 0x3C, 0x3C, 0x18, 0x00, 0x00]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_changes_from_top_to_bottom() {
        let width = 4;
        let height = 8;
        let buf = fill_gradient(width, height);
        assert_eq!(&buf[0..3], &[48, 36, 28]);
        let last = ((height - 1) * width * 4) as usize;
        assert_eq!(&buf[last..last + 3], &[58, 72, 62]);
        assert_ne!(&buf[0..3], &buf[last..last + 3]);
    }

    #[test]
    fn collapsed_strip_ignores_clicks_past_the_icons() {
        assert_eq!(hit_strip(false, 600, 10.0, 10.0), Some(Entry::Start));
        assert_eq!(hit_strip(false, 600, 10.0, 60.0), Some(Entry::Browser));
        assert_eq!(hit_strip(false, 600, 10.0, 520.0), Some(Entry::Network));
        assert_eq!(hit_strip(false, 600, 10.0, 580.0), Some(Entry::Power));
        assert_eq!(hit_strip(false, 600, 49.0, 10.0), None);
        assert_eq!(hit_strip(true, 600, 180.0, 60.0), Some(Entry::Browser));
    }

    #[test]
    fn power_menu_hits_the_three_actions() {
        let (x, y) = power_menu_origin(600);
        assert_eq!(hit_power_action(600, f64::from(x + 8), f64::from(y + 8)), Some(PowerAction::Logout));
        assert_eq!(
            hit_power_action(600, f64::from(x + 8), f64::from(y + 48 + 8)),
            Some(PowerAction::Shutdown)
        );
        assert_eq!(
            hit_power_action(600, f64::from(x + 8), f64::from(y + 96 + 8)),
            Some(PowerAction::Reboot)
        );
        assert_eq!(hit_power_action(600, 0.0, 0.0), None);
        assert_eq!(hit_power_action(600, f64::from(x + 8), f64::from(y + 40)), None);
    }

    #[test]
    fn strip_buffer_covers_the_panel() {
        let buf = paint_strip(STRIP_COLLAPSED, 100, false, Page::Start, false, false);
        assert_eq!(buf.len(), (STRIP_COLLAPSED * 100 * 4) as usize);
    }

    #[test]
    fn content_starts_beside_the_resting_strip() {
        assert_eq!(content_origin(), (STRIP_COLLAPSED, 0));
        assert_eq!(content_size(1024, 600), (1024 - STRIP_COLLAPSED, 600));
    }

    #[test]
    fn cursor_is_antialiased_and_the_hotspot_sits_on_the_tip() {
        let cursor = paint_cursor();
        assert_eq!((cursor.width, cursor.height), (32, 32));
        assert_eq!((cursor.hot_x, cursor.hot_y), (5, 2));
        let tip = ((cursor.hot_y * cursor.width + cursor.hot_x) * 4) as usize;
        assert_eq!(cursor.pixels[tip + 3], 255);
        assert!(cursor.pixels.chunks(4).any(|pixel| pixel[3] > 0 && pixel[3] < 255));
    }

    #[test]
    fn startup_mark_moves_without_claiming_a_fraction() {
        assert_eq!(startup_phase(0), 0.0);
        assert!(startup_phase(700) > startup_phase(0));
        assert!(startup_phase(1_400) < 0.01);
        let early = paint_startup_bar(0.0);
        let later = paint_startup_bar(0.6);
        assert_eq!(early.len(), (BAR_W * BAR_H * 4) as usize);
        assert_ne!(early, later);
    }

    #[test]
    fn start_and_browser_icons_differ_and_are_antialiased() {
        let buf = paint_strip(STRIP_COLLAPSED, 600, false, Page::Start, false, false);
        let start = ((24 * STRIP_COLLAPSED + 24) * 4) as usize;
        let browser = ((72 * STRIP_COLLAPSED + 24) * 4) as usize;
        assert_ne!(&buf[start..start + 3], &buf[browser..browser + 3]);
        let menu = paint_power_menu();
        assert_eq!(menu.len(), (MENU_W * MENU_H * 4) as usize);
        let row = |y: i32| {
            let start = (y * MENU_W * 4) as usize;
            menu[start..start + (MENU_W * 4) as usize].to_vec()
        };
        assert_ne!(row(20), row(68));
    }

    #[test]
    fn wifi_panel_hits_toggle_scan_row_and_join() {
        let (x, y) = wifi_origin(600);
        assert_eq!(hit_wifi(600, f64::from(x + WIFI_W - 20), f64::from(y + 10), 3, 0), Some(WifiHit::Toggle));
        assert_eq!(hit_wifi(600, f64::from(x + 80), f64::from(y + 10), 3, 0), Some(WifiHit::Scan));
        let list_y = y + WIFI_HEADER + WIFI_STATUS + 4;
        assert_eq!(hit_wifi(600, f64::from(x + 40), f64::from(list_y), 3, 0), Some(WifiHit::Row(0)));
        assert_eq!(
            hit_wifi(600, f64::from(x + WIFI_W - 10), f64::from(list_y + WIFI_ROW), 3, 1),
            Some(WifiHit::Disconnect(2))
        );
        assert_eq!(
            hit_wifi(600, f64::from(x + WIFI_W - 20), f64::from(y + WIFI_H - 10), 3, 0),
            Some(WifiHit::Join)
        );
        assert_eq!(clamp_wifi_scroll(-2, 10), 0);
        assert_eq!(clamp_wifi_scroll(9, 8), 2);
        let panel = paint_wifi_panel(
            &[WifiPaint { name: "Cafe", signal: 80, secured: true, connected: false }],
            0,
            Some(true),
            "1 networks",
            Some("secret"),
        );
        assert_eq!(panel.len(), (WIFI_W * WIFI_H * 4) as usize);
        let long = "A".repeat(40);
        let named = paint_wifi_panel(
            &[WifiPaint { name: &long, signal: 80, secured: false, connected: false }],
            0,
            Some(true),
            "ok",
            None,
        );
        let drawn = ((68 * WIFI_W + 34) * 4) as usize;
        assert_eq!(&named[drawn..drawn + 3], &[216, 226, 230]);
        let kept = ((59 * WIFI_W + 14) * 4) as usize;
        assert_eq!(&named[kept..kept + 3], &[29, 24, 22]);
    }
}

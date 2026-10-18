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

/// Approaches nine tenths and stops there until the browser window exists.
pub fn startup_fraction(elapsed_ms: u128) -> f32 {
    let seconds = elapsed_ms as f32 / 1000.0;
    (1.0 - (-seconds / 5.0).exp()) * 0.9
}

pub fn hit_strip(expanded: bool, x: f64, y: f64) -> Option<Entry> {
    if x < 0.0 || y < 0.0 || x >= f64::from(strip_width(expanded)) {
        return None;
    }
    let row = (y / f64::from(ROW)) as i32;
    match row {
        0 => Some(Entry::Start),
        1 => Some(Entry::Browser),
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

pub fn paint_strip(width: i32, height: i32, expanded: bool, page: Page) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            put(&mut pixels, width, x, y, 22, 24, 29);
        }
    }
    for (index, entry) in [Entry::Start, Entry::Browser].into_iter().enumerate() {
        let row_y = index as i32 * ROW;
        if row_y >= height {
            break;
        }
        let active = match (page, entry) {
            (Page::Start, Entry::Start) | (Page::Browser, Entry::Browser) => true,
            _ => false,
        };
        if active {
            fill_rect(&mut pixels, width, height, 0, row_y, 3, ROW, 215, 196, 163);
        }
        paint_entry_icon(&mut pixels, width, height, 10, row_y + 10, entry);
        if expanded {
            let label = match entry {
                Entry::Start => "Start",
                Entry::Browser => "Browser",
            };
            draw_text(&mut pixels, width, height, 48, row_y + 16, label);
        }
    }
    pixels
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
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
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

/// Arrow with a dark outline and a light fill. The tip is the hotspot.
pub fn paint_cursor() -> CursorImage {
    const ROWS: [&str; 21] = [
        "#",
        "##",
        "#.#",
        "#..#",
        "#...#",
        "#....#",
        "#.....#",
        "#......#",
        "#.......#",
        "#........#",
        "#......####",
        "#...#..#",
        "#..#..#",
        "#.#..#",
        "##..#",
        "#..#",
        " ..#",
        "  ..#",
        "   ..#",
        "    ..#",
        "     ##",
    ];
    let height = ROWS.len() as i32;
    let width = ROWS.iter().map(|row| row.len()).max().unwrap_or(1) as i32;
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for (y, row) in ROWS.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            match ch {
                '#' => put(&mut pixels, width, x as i32, y as i32, 20, 20, 22),
                '.' => put(&mut pixels, width, x as i32, y as i32, 245, 244, 236),
                _ => {}
            }
        }
    }
    CursorImage {
        pixels,
        width,
        height,
        hot_x: 0,
        hot_y: 0,
    }
}

pub fn paint_startup_bar(fraction: f32) -> Vec<u8> {
    let fraction = fraction.clamp(0.0, 1.0);
    let mut pixels = vec![0u8; (BAR_W * BAR_H * 4) as usize];
    fill_rect(&mut pixels, BAR_W, BAR_H, 0, 0, BAR_W, BAR_H, 22, 24, 29);
    let label = "Starting";
    let text_w = label.len() as i32 * 6;
    draw_text(&mut pixels, BAR_W, BAR_H, (BAR_W - text_w) / 2, 4, label);
    let track_y = 20;
    fill_rect(&mut pixels, BAR_W, BAR_H, 8, track_y, BAR_W - 16, 6, 12, 13, 16);
    let fill = ((BAR_W - 16) as f32 * fraction).round() as i32;
    if fill > 0 {
        fill_rect(&mut pixels, BAR_W, BAR_H, 8, track_y, fill, 6, 215, 196, 163);
    }
    pixels
}

fn paint_entry_icon(buf: &mut [u8], width: i32, height: i32, x: i32, y: i32, entry: Entry) {
    let plate = match entry {
        Entry::Start => (58, 48, 36),
        Entry::Browser => (36, 48, 60),
    };
    let mark = match entry {
        Entry::Start => (236, 220, 176),
        Entry::Browser => (232, 236, 240),
    };
    rounded_rect(buf, width, height, x, y, 28, 28, 6, plate.0, plate.1, plate.2);
    match entry {
        Entry::Start => star(buf, width, height, x + 14, y + 14, mark.0, mark.1, mark.2),
        Entry::Browser => globe(buf, width, height, x + 14, y + 14, mark.0, mark.1, mark.2),
    }
}

fn rounded_rect(buf: &mut [u8], width: i32, _height: i32, x: i32, y: i32, w: i32, h: i32, radius: i32, r: u8, g: u8, b: u8) {
    for dy in 0..h {
        for dx in 0..w {
            if inside_round(dx, dy, w, h, radius) {
                put(buf, width, x + dx, y + dy, r, g, b);
            }
        }
    }
}

fn inside_round(dx: i32, dy: i32, w: i32, h: i32, radius: i32) -> bool {
    let (cx, cy) = if dx < radius && dy < radius {
        (radius - 1, radius - 1)
    } else if dx >= w - radius && dy < radius {
        (w - radius, radius - 1)
    } else if dx < radius && dy >= h - radius {
        (radius - 1, h - radius)
    } else if dx >= w - radius && dy >= h - radius {
        (w - radius, h - radius)
    } else {
        return true;
    };
    let ox = dx - cx;
    let oy = dy - cy;
    ox * ox + oy * oy <= radius * radius
}

fn star(buf: &mut [u8], width: i32, _height: i32, cx: i32, cy: i32, r: u8, g: u8, b: u8) {
    for dy in -9..=9i32 {
        for dx in -9..=9i32 {
            let ax = dx.abs();
            let ay = dy.abs();
            let arm = (ay <= 1 && ax <= 8) || (ax <= 1 && ay <= 8) || (ax + ay <= 5);
            if arm {
                put(buf, width, cx + dx, cy + dy, r, g, b);
            }
        }
    }
}

fn globe(buf: &mut [u8], width: i32, _height: i32, cx: i32, cy: i32, r: u8, g: u8, b: u8) {
    for dy in -8..=8i32 {
        for dx in -8..=8i32 {
            let d2 = dx * dx + dy * dy;
            let ring = (36..=64).contains(&d2);
            let equator = dy.abs() <= 1 && d2 <= 64;
            let meridian = dx.abs() <= 1 && d2 <= 64;
            if ring || equator || meridian {
                put(buf, width, cx + dx, cy + dy, r, g, b);
            }
        }
    }
}

fn blend(from: u8, to: u8, y: u32, last: u32) -> u8 {
    let v = u32::from(from) * (last - y) + u32::from(to) * y;
    (v / last) as u8
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
        assert_eq!(hit_strip(false, 10.0, 10.0), Some(Entry::Start));
        assert_eq!(hit_strip(false, 10.0, 60.0), Some(Entry::Browser));
        assert_eq!(hit_strip(false, 49.0, 10.0), None);
        assert_eq!(hit_strip(true, 180.0, 60.0), Some(Entry::Browser));
    }

    #[test]
    fn strip_buffer_covers_the_panel() {
        let buf = paint_strip(STRIP_COLLAPSED, 100, false, Page::Start);
        assert_eq!(buf.len(), (STRIP_COLLAPSED * 100 * 4) as usize);
    }

    #[test]
    fn content_starts_beside_the_resting_strip() {
        assert_eq!(content_origin(), (STRIP_COLLAPSED, 0));
        assert_eq!(content_size(1024, 600), (1024 - STRIP_COLLAPSED, 600));
    }

    #[test]
    fn cursor_tip_is_opaque_and_the_opposite_corner_is_clear() {
        let cursor = paint_cursor();
        assert_eq!(&cursor.pixels[0..4], &[22, 20, 20, 255]);
        let last = cursor.pixels.len() - 4;
        assert_eq!(&cursor.pixels[last..], &[0, 0, 0, 0]);
        assert_eq!((cursor.hot_x, cursor.hot_y), (0, 0));
    }

    #[test]
    fn startup_bar_fills_without_reaching_the_end() {
        assert!(startup_fraction(0) < 0.05);
        assert!(startup_fraction(8_000) > startup_fraction(1_000));
        assert!(startup_fraction(120_000) < 1.0);
        let bar = paint_startup_bar(0.5);
        assert_eq!(bar.len(), (BAR_W * BAR_H * 4) as usize);
    }

    #[test]
    fn start_and_browser_icons_differ() {
        let buf = paint_strip(STRIP_COLLAPSED, 96, false, Page::Start);
        let start = ((24 * STRIP_COLLAPSED + 24) * 4) as usize;
        let browser = ((72 * STRIP_COLLAPSED + 24) * 4) as usize;
        assert_ne!(&buf[start..start + 3], &buf[browser..browser + 3]);
    }
}

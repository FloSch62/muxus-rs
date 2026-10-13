//! The invented desktop of the documentation sandbox (`hack/demo-vnc.mjs` `drawDesktop`),
//! drawn from rectangles so the remote-desktop screenshots contain no real host.

/// An RGB colour.
type Rgb = [u8; 3];

fn hex(value: &str) -> Rgb {
    let channel = |at: usize| u8::from_str_radix(&value[at..at + 2], 16).unwrap();
    [channel(1), channel(3), channel(5)]
}

/// `Math.round` for the positive values the drawing uses.
fn round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let channel = |i: usize| {
        let a = f64::from(a[i]);
        round(a + (f64::from(b[i]) - a) * t) as u8
    };
    [channel(0), channel(1), channel(2)]
}

/// Row-major RGB pixels, `width * height * 3` bytes.
pub struct Desktop {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Desktop {
    fn set(&mut self, x: i64, y: i64, colour: Rgb) {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return;
        }
        let at = (y as usize * self.width as usize + x as usize) * 3;
        self.pixels[at..at + 3].copy_from_slice(&colour);
    }

    fn rect(&mut self, x: i64, y: i64, w: i64, h: i64, colour: Rgb, radius: i64) {
        for row in 0..h {
            for col in 0..w {
                if radius != 0 {
                    let dx = (radius - col).max(col - (w - 1 - radius)).max(0);
                    let dy = (radius - row).max(row - (h - 1 - radius)).max(0);
                    if dx * dx + dy * dy > radius * radius {
                        continue;
                    }
                }
                self.set(x + col, y + row, colour);
            }
        }
    }

    fn circle(&mut self, cx: i64, cy: i64, r: i64, colour: Rgb) {
        for y in -r..=r {
            for x in -r..=r {
                if x * x + y * y <= r * r {
                    self.set(cx + x, cy + y, colour);
                }
            }
        }
    }

    /// The pixel at (x, y) as B, G, R, A.
    pub fn bgra(&self) -> Vec<u8> {
        self.pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|rgb| [rgb[2], rgb[1], rgb[0], 255])
            .collect()
    }
}

pub fn draw(width: u32, height: u32) -> Desktop {
    let mut d = Desktop {
        width,
        height,
        pixels: vec![0; width as usize * height as usize * 3],
    };
    let (w, h) = (i64::from(width), i64::from(height));
    let (wf, hf) = (f64::from(width), f64::from(height));

    // Wallpaper: a deep blue to teal gradient with a soft diagonal band.
    let top = hex("#10224a");
    let bottom = hex("#15707c");
    let band_colour = hex("#3aa6b9");
    for y in 0..h {
        for x in 0..w {
            let band =
                (1.0 - ((x as f64 - y as f64 * 1.4) - wf * 0.35).abs() / (wf * 0.35)).max(0.0);
            let colour = mix(mix(top, bottom, y as f64 / hf), band_colour, band * 0.18);
            d.set(x, y, colour);
        }
    }

    // Desktop icons.
    for (i, colour) in ["#60a5fa", "#fbbf24", "#34d399"].into_iter().enumerate() {
        let i = i as i64;
        d.rect(28, 28 + i * 86, 48, 40, hex(colour), 6);
        d.rect(34, 76 + i * 86, 36, 5, hex("#dbe7f5"), 2);
    }

    // A dashboard window.
    let wx = 150;
    let wy = 70;
    let ww = round(wf * 0.58);
    let wh = round(hf * 0.66);
    d.rect(wx + 6, wy + 8, ww, wh, hex("#0b1830"), 10);
    d.rect(wx, wy, ww, wh, hex("#f8fafc"), 10);
    d.rect(wx, wy, ww, 36, hex("#e2e8f0"), 10);
    d.rect(wx, wy + 26, ww, 10, hex("#e2e8f0"), 0);
    d.rect(wx + 16, wy + 13, 160, 10, hex("#94a3b8"), 5);
    for (i, colour) in ["#94a3b8", "#94a3b8", "#ef4444"].into_iter().enumerate() {
        d.circle(wx + ww - 70 + i as i64 * 24, wy + 18, 6, hex(colour));
    }
    d.rect(wx, wy + 36, 170, wh - 36, hex("#eef2f7"), 0);
    for i in 0..7i64 {
        let width = if i == 1 { 120 } else { 96 + (i % 3) * 14 };
        let colour = if i == 1 { "#6366f1" } else { "#b6c2d2" };
        d.rect(wx + 20, wy + 64 + i * 34, width, 10, hex(colour), 5);
    }
    let cx = wx + 200;
    let cw = ww - 230;
    let third = round(cw as f64 / 3.0);
    for (i, colour) in ["#6366f1", "#22c55e", "#f59e0b"].into_iter().enumerate() {
        let x = cx + i as i64 * third;
        d.rect(x, wy + 60, third - 16, 70, hex("#ffffff"), 8);
        d.rect(x + 14, wy + 76, 60, 8, hex("#cbd5e1"), 4);
        d.rect(x + 14, wy + 96, 90, 18, hex(colour), 5);
    }
    let chart_top = wy + 150;
    let chart_height = wh - 190;
    d.rect(cx, chart_top, cw - 16, chart_height, hex("#ffffff"), 8);
    let bars = [
        0.42, 0.58, 0.51, 0.72, 0.64, 0.81, 0.69, 0.88, 0.77, 0.93, 0.84, 0.97,
    ];
    let bar_width = ((cw - 60) as f64 / bars.len() as f64).floor() as i64 - 8;
    for (i, value) in bars.iter().enumerate() {
        let height = round((chart_height - 50) as f64 * value);
        let colour = if i == bars.len() - 1 {
            "#6366f1"
        } else {
            "#a5b4fc"
        };
        d.rect(
            cx + 22 + i as i64 * (bar_width + 8),
            chart_top + chart_height - 22 - height,
            bar_width,
            height,
            hex(colour),
            4,
        );
    }

    // A terminal window overlapping it.
    let tx = wx + ww - 150;
    let ty = wy + wh - 190;
    let tw = round(wf * 0.36);
    let th = round(hf * 0.38);
    d.rect(tx + 6, ty + 8, tw, th, hex("#06101f"), 10);
    d.rect(tx, ty, tw, th, hex("#1e2430"), 10);
    d.rect(tx, ty, tw, 30, hex("#2b3342"), 10);
    d.rect(tx, ty + 22, tw, 8, hex("#2b3342"), 0);
    for (i, colour) in ["#ef4444", "#f59e0b", "#22c55e"].into_iter().enumerate() {
        d.circle(tx + 20 + i as i64 * 20, ty + 15, 5, hex(colour));
    }
    let lines = [
        (0.55, "#86efac"),
        (0.8, "#cbd5e1"),
        (0.62, "#cbd5e1"),
        (0.35, "#93c5fd"),
        (0.7, "#cbd5e1"),
        (0.45, "#86efac"),
        (0.25, "#cbd5e1"),
    ];
    for (i, (fraction, colour)) in lines.into_iter().enumerate() {
        d.rect(
            tx + 18,
            ty + 48 + i as i64 * 26,
            round((tw - 40) as f64 * fraction),
            9,
            hex(colour),
            4,
        );
    }

    // Taskbar.
    d.rect(0, h - 44, w, 44, hex("#0c1424"), 0);
    d.rect(12, h - 34, 24, 24, hex("#6366f1"), 5);
    for (i, colour) in ["#60a5fa", "#f8fafc", "#34d399", "#fbbf24"]
        .into_iter()
        .enumerate()
    {
        d.rect(56 + i as i64 * 40, h - 34, 24, 24, hex(colour), 5);
    }
    d.rect(w - 96, h - 28, 80, 12, hex("#64748b"), 6);
    d
}

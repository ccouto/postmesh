//! Legend, caption and axes triad as backend-independent primitives (pixel coordinates,
//! sizes relative to the image height so every output looks the same).

use crate::colormap;
use crate::gidio::fmt_g;
use image::RgbaImage;

#[derive(Clone, Debug)]
pub enum Prim {
    Rect { x: f32, y: f32, w: f32, h: f32, col: [u8; 3] },
    Line { a: [f32; 2], b: [f32; 2], col: [u8; 3], width: f32 },
    /// `(x, y)` is the top-left corner of the text box.
    Text { x: f32, y: f32, size: f32, col: [u8; 3], text: String },
}

pub struct OverlayInfo {
    pub title: String,
    pub caption: Vec<String>,
    pub clim: (f64, f64),
    pub colormap: String,
    pub have_result: bool,
    pub show_legend: bool,
    pub show_caption: bool,
    pub show_axes: bool,
    /// screen directions (x right, y down; unit-ish) of world X, Y, Z
    pub axes: [[f32; 2]; 3],
}

pub fn layout(w: f32, h: f32, info: &OverlayInfo) -> Vec<Prim> {
    let mut out = Vec::new();
    let black = [0u8, 0, 0];
    if info.have_result && info.show_legend {
        // colour bar
        let bar_w = 0.028 * h;
        let bar_h = 0.70 * h;
        let title_w = info.title.chars().count() as f32 * 0.021 * h * 0.52;
        let bar_x = (w - (title_w.max(0.17 * h) + 0.03 * h)).max(w * 0.5);
        let bar_y = 0.15 * h;
        let nb = 128usize;
        for i in 0..nb {
            // i = 0 is the top (max)
            let band = nb - 1 - i;
            let col = colormap::to_u8(colormap::sample(&info.colormap, band as f32 / (nb - 1) as f32));
            let y0 = bar_y + bar_h * i as f32 / nb as f32;
            let y1 = bar_y + bar_h * (i + 1) as f32 / nb as f32;
            out.push(Rect_(bar_x, y0, bar_w, (y1 - y0) + 0.6, col));
        }
        let fs = 0.018 * h;
        let n_labels = 11;
        for i in 0..n_labels {
            let f = i as f64 / (n_labels - 1) as f64;
            let v = info.clim.1 - (info.clim.1 - info.clim.0) * f;
            let y = bar_y + bar_h * f as f32;
            out.push(Prim::Line { a: [bar_x + bar_w, y], b: [bar_x + bar_w + 0.006 * h, y], col: black, width: (0.0012 * h).max(1.0) });
            out.push(Prim::Text {
                x: bar_x + bar_w + 0.009 * h,
                y: y - fs * 0.6,
                size: fs,
                col: black,
                text: fmt_g(v, 4),
            });
        }
        out.push(Prim::Text { x: bar_x, y: bar_y - 0.04 * h, size: 0.021 * h, col: black, text: info.title.clone() });

    }
    if info.have_result && info.show_caption {
        // caption, bottom-left
        let cs = 0.019 * h;
        let n = info.caption.len() as f32;
        for (i, l) in info.caption.iter().enumerate() {
            out.push(Prim::Text {
                x: 0.012 * h,
                y: h - 0.012 * h - cs * 1.25 * (n - i as f32),
                size: cs,
                col: black,
                text: l.clone(),
            });
        }
    }

    if !info.show_axes {
        return out;
    }
    // axes triad above the caption
    let len = 0.06 * h;
    let c = [0.08 * h, h - 0.17 * h];
    let cols = [[200u8, 40, 40], [40, 160, 70], [40, 70, 210]];
    let names = ["X", "Y", "Z"];
    for k in 0..3 {
        let d = info.axes[k];
        let tip = [c[0] + d[0] * len, c[1] + d[1] * len];
        out.push(Prim::Line { a: c, b: tip, col: cols[k], width: (0.004 * h).max(1.5) });
        out.push(Prim::Text {
            x: c[0] + d[0] * len * 1.18 - 0.007 * h,
            y: c[1] + d[1] * len * 1.18 - 0.011 * h,
            size: 0.020 * h,
            col: cols[k],
            text: names[k].into(),
        });
    }
    out
}

#[allow(non_snake_case)]
fn Rect_(x: f32, y: f32, w: f32, h: f32, col: [u8; 3]) -> Prim {
    Prim::Rect { x, y, w, h, col }
}

// ------------------------------------------------------------------ raster backend

pub fn default_font_bytes() -> Vec<u8> {
    let defs = eframe::egui::FontDefinitions::default();
    let name = defs
        .families
        .get(&eframe::egui::FontFamily::Proportional)
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or_else(|| "Ubuntu-Light".into());
    defs.font_data.get(&name).map(|d| (*d.font).as_ref().to_vec()).unwrap_or_default()
}

pub fn draw_raster(img: &mut RgbaImage, prims: &[Prim]) {
    use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
    let bytes = default_font_bytes();
    let font = FontRef::try_from_slice(&bytes).ok();
    for p in prims {
        match p {
            Prim::Rect { x, y, w, h, col } => {
                let (x0, y0) = (x.floor() as i32, y.floor() as i32);
                let (x1, y1) = ((x + w).ceil() as i32, (y + h).ceil() as i32);
                for yy in y0.max(0)..y1.min(img.height() as i32) {
                    for xx in x0.max(0)..x1.min(img.width() as i32) {
                        img.put_pixel(xx as u32, yy as u32, image::Rgba([col[0], col[1], col[2], 255]));
                    }
                }
            }
            Prim::Line { a, b, col, width } => {
                thick_line(img, *a, *b, *width, [col[0], col[1], col[2], 255]);
            }
            Prim::Text { x, y, size, col, text } => {
                let Some(font) = &font else { continue };
                // egui lays out text in a box of height `size` with the glyph origin at the ascent
                let scale = PxScale::from(*size * 1.0);
                let sf = font.as_scaled(scale);
                let mut pen = *x;
                let base = *y + sf.ascent() * 0.98;
                let mut prev = None;
                for ch in text.chars() {
                    let gid = font.glyph_id(ch);
                    if let Some(pg) = prev {
                        pen += sf.kern(pg, gid);
                    }
                    let glyph = gid.with_scale_and_position(scale, ab_glyph::point(pen, base));
                    pen += sf.h_advance(gid);
                    prev = Some(gid);
                    if let Some(og) = font.outline_glyph(glyph) {
                        let bb = og.px_bounds();
                        og.draw(|gx, gy, cov| {
                            let px = bb.min.x as i32 + gx as i32;
                            let py = bb.min.y as i32 + gy as i32;
                            if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                                let dst = img.get_pixel_mut(px as u32, py as u32);
                                let a = cov;
                                let da = dst[3] as f32 / 255.0;
                                let oa = a + da * (1.0 - a);
                                for k in 0..3 {
                                    let c = col[k] as f32 * a + dst[k] as f32 * da * (1.0 - a);
                                    dst[k] = if oa > 0.0 { (c / oa).round().clamp(0.0, 255.0) as u8 } else { 0 };
                                }
                                dst[3] = (oa * 255.0).round() as u8;
                            }
                        });
                    }
                }
            }
        }
    }
}

fn thick_line(img: &mut RgbaImage, a: [f32; 2], b: [f32; 2], width: f32, col: [u8; 4]) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let r = (width * 0.5).max(0.5);
    let minx = (a[0].min(b[0]) - r).floor().max(0.0) as i32;
    let maxx = (a[0].max(b[0]) + r).ceil().min(img.width() as f32 - 1.0) as i32;
    let miny = (a[1].min(b[1]) - r).floor().max(0.0) as i32;
    let maxy = (a[1].max(b[1]) + r).ceil().min(img.height() as f32 - 1.0) as i32;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let (px, py) = (x as f32 + 0.5 - a[0], y as f32 + 0.5 - a[1]);
            let t = ((px * dx + py * dy) / (len * len)).clamp(0.0, 1.0);
            let (cx, cy) = (px - t * dx, py - t * dy);
            let d = (cx * cx + cy * cy).sqrt();
            let cov = (r + 0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                let dst = img.get_pixel_mut(x as u32, y as u32);
                let da = dst[3] as f32 / 255.0;
                let oa = cov + da * (1.0 - cov);
                for k in 0..3 {
                    let c = col[k] as f32 * cov + dst[k] as f32 * da * (1.0 - cov);
                    dst[k] = if oa > 0.0 { (c / oa).round().clamp(0.0, 255.0) as u8 } else { 0 };
                }
                dst[3] = (oa * 255.0).round() as u8;
            }
        }
    }
}

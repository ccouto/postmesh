//! Vector export (SVG / PDF / EPS / PS). The surface is clipped into fine iso-value polygons
//! and painted back-to-front, so the output stays sharp at any zoom.

use crate::colormap;
use crate::doc::Surface;
use crate::overlay::Prim;
use anyhow::{Result, bail};
use flate2::{Compression, write::ZlibEncoder};
use glam::{Mat4, Vec4};
use std::fmt::Write as _;
use std::io::Write as _;

pub struct VectorScene<'a> {
    pub w: f32,
    pub h: f32,
    pub mvp: Mat4,
    pub positions: &'a [[f32; 3]],
    pub rest: &'a [[f32; 3]],
    pub scalars: &'a [f32],
    pub surface: &'a Surface,
    pub clim: (f32, f32),
    pub colormap: &'a str,
    pub have_result: bool,
    pub show_edges: bool,
    pub show_outline: bool,
    pub show_ghost: bool,
    pub transparent: bool,
    pub overlay: Vec<Prim>,
}

enum V {
    Poly { pts: Vec<[f32; 2]>, fill: [u8; 3], stroke: Option<([u8; 3], f32)> },
    Outline { pts: Vec<[f32; 2]>, col: [u8; 3], width: f32 },
    Overlay(Prim),
    Line { a: [f32; 2], b: [f32; 2], col: [u8; 3], width: f32 },
}

#[derive(Clone, Copy)]
struct P {
    x: f32,
    y: f32,
    t: f32,
}

fn clip(poly: &[P], lo: f32, keep_ge: bool) -> Vec<P> {
    let inside = |p: &P| if keep_ge { p.t >= lo } else { p.t <= lo };
    let mut out = Vec::with_capacity(poly.len() + 2);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (ia, ib) = (inside(&a), inside(&b));
        if ia {
            out.push(a);
        }
        if ia != ib {
            let f = (lo - a.t) / (b.t - a.t);
            out.push(P { x: a.x + (b.x - a.x) * f, y: a.y + (b.y - a.y) * f, t: lo });
        }
    }
    out
}

fn build(s: &VectorScene) -> Vec<V> {
    let proj = |p: &[f32; 3]| -> Option<(f32, f32, f32)> {
        let c = s.mvp * Vec4::new(p[0], p[1], p[2], 1.0);
        if c.w <= 1e-9 {
            return None;
        }
        let (nx, ny, nz) = (c.x / c.w, c.y / c.w, c.z / c.w);
        Some(((nx * 0.5 + 0.5) * s.w, (1.0 - (ny * 0.5 + 0.5)) * s.h, nz))
    };
    let mut out = Vec::new();
    if s.show_ghost {
        let pr: Vec<_> = s.rest.iter().map(proj).collect();
        for e in &s.surface.feature_edges {
            if let (Some(a), Some(b)) = (pr[e[0] as usize], pr[e[1] as usize]) {
                out.push(V::Line { a: [a.0, a.1], b: [b.0, b.1], col: [140, 140, 140], width: 0.6 });
            }
        }
    }
    let pr: Vec<_> = s.positions.iter().map(proj).collect();
    let (lo, hi) = s.clim;
    let span = (hi - lo).max(1e-30);
    let nlev = 128usize;
    let tval = |sc: f32| (sc - lo) / span;

    let feature: std::collections::HashSet<(u32, u32)> = if s.show_outline && !s.show_edges {
        s.surface.feature_edges.iter().map(|e| (e[0].min(e[1]), e[0].max(e[1]))).collect()
    } else {
        Default::default()
    };
    let mut faces: Vec<(f32, usize)> = Vec::with_capacity(s.surface.polys.len());
    for (i, f) in s.surface.polys.iter().enumerate() {
        let n = crate::doc::poly_len(f);
        let mut z = 0.0;
        let mut ok = true;
        for k in 0..n {
            match pr[f[k] as usize] {
                Some(p) => z += p.2,
                None => ok = false,
            }
        }
        if ok {
            faces.push((z / n as f32, i));
        }
    }
    faces.sort_by(|a, b| b.0.total_cmp(&a.0)); // far first

    for &(_, fi) in &faces {
        let f = s.surface.polys[fi];
        let n = crate::doc::poly_len(&f);
        let pt = |k: usize| {
            let p = pr[f[k] as usize].unwrap();
            P { x: p.0, y: p.1, t: tval(s.scalars[f[k] as usize]) }
        };
        let tris: &[[usize; 3]] = if n == 3 { &[[0, 1, 2]] } else { &[[0, 1, 2], [0, 2, 3]] };
        for tri in tris {
            let v = [pt(tri[0]), pt(tri[1]), pt(tri[2])];
            if !s.have_result {
                out.push(V::Poly {
                    pts: v.iter().map(|p| [p.x, p.y]).collect(),
                    fill: [176, 196, 222],
                    stroke: Some(([176, 196, 222], 0.4)),
                });
                continue;
            }
            let tmin = v.iter().map(|p| p.t).fold(f32::MAX, f32::min);
            let tmax = v.iter().map(|p| p.t).fold(f32::MIN, f32::max);
            let band = |t: f32| ((t * nlev as f32).floor().max(0.0) as usize).min(nlev - 1);
            for k in band(tmin)..=band(tmax) {
                let mut poly = v.to_vec();
                if k > 0 {
                    poly = clip(&poly, k as f32 / nlev as f32, true);
                }
                if k + 1 < nlev && poly.len() >= 3 {
                    poly = clip(&poly, (k + 1) as f32 / nlev as f32, false);
                }
                if poly.len() < 3 {
                    continue;
                }
                let fill = colormap::to_u8(colormap::sample(s.colormap, (k as f32 + 0.5) / nlev as f32));
                out.push(V::Poly {
                    pts: poly.iter().map(|p| [p.x, p.y]).collect(),
                    fill,
                    stroke: Some((fill, 0.35)),
                });
            }
        }
        if !feature.is_empty() {
            for k in 0..n {
                let (a, b) = (f[k], f[(k + 1) % n]);
                if feature.contains(&(a.min(b), a.max(b))) {
                    let (pa, pb) = (pr[a as usize].unwrap(), pr[b as usize].unwrap());
                    out.push(V::Line { a: [pa.0, pa.1], b: [pb.0, pb.1], col: [0, 0, 0], width: 0.8 });
                }
            }
        }
        if s.show_edges {
            out.push(V::Outline { pts: (0..n).map(|k| {
                let p = pr[f[k] as usize].unwrap();
                [p.0, p.1]
            }).collect(), col: [0, 0, 0], width: 0.3 });
        }
    }
    out.extend(s.overlay.iter().cloned().map(V::Overlay));
    out
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn svg(s: &VectorScene, items: &[V]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        o,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#,
        w = s.w,
        h = s.h
    );
    if !s.transparent {
        let _ = writeln!(o, r##"<rect width="100%" height="100%" fill="#ffffff"/>"##);
    }
    let pts = |p: &[[f32; 2]]| p.iter().map(|q| format!("{:.2},{:.2}", q[0], q[1])).collect::<Vec<_>>().join(" ");
    for it in items {
        match it {
            V::Poly { pts: p, fill, stroke } => {
                let st = stroke
                    .map(|(c, w)| format!(r#" stroke="{}" stroke-width="{w}" stroke-linejoin="round""#, hex(c)))
                    .unwrap_or_default();
                let _ = writeln!(o, r#"<polygon points="{}" fill="{}"{st}/>"#, pts(p), hex(*fill));
            }
            V::Outline { pts: p, col, width } => {
                let _ = writeln!(o, r#"<polygon points="{}" fill="none" stroke="{}" stroke-width="{width}"/>"#, pts(p), hex(*col));
            }
            V::Line { a, b, col, width } => {
                let _ = writeln!(
                    o,
                    r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="{width}"/>"#,
                    a[0], a[1], b[0], b[1], hex(*col)
                );
            }
            V::Overlay(Prim::Rect { x, y, w, h, col }) => {
                let _ = writeln!(o, r#"<rect x="{x:.2}" y="{y:.2}" width="{w:.2}" height="{h:.2}" fill="{}"/>"#, hex(*col));
            }
            V::Overlay(Prim::Line { a, b, col, width }) => {
                let _ = writeln!(
                    o,
                    r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="{width:.2}"/>"#,
                    a[0], a[1], b[0], b[1], hex(*col)
                );
            }
            V::Overlay(Prim::Text { x, y, size, col, text }) => {
                let _ = writeln!(
                    o,
                    r#"<text x="{x:.2}" y="{:.2}" font-family="Helvetica, Arial, sans-serif" font-size="{size:.2}" fill="{}">{}</text>"#,
                    y + size * 0.82,
                    hex(*col),
                    xml(text)
                );
            }
        }
    }
    o.push_str("</svg>\n");
    o
}

fn rgb01(c: [u8; 3]) -> String {
    format!("{:.3} {:.3} {:.3}", c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0)
}

fn ps_escape(s: &str) -> String {
    let mut o = String::new();
    for ch in s.chars() {
        match ch {
            '(' | ')' | '\\' => {
                o.push('\\');
                o.push(ch);
            }
            c if c.is_ascii() && !c.is_control() => o.push(c),
            _ => o.push('?'),
        }
    }
    o
}

/// PDF/PostScript share the same painting model; `pdf` selects operators and y-flip.
fn paint(s: &VectorScene, items: &[V], pdf: bool) -> String {
    let h = s.h;
    let y = |v: f32| h - v;
    let mut o = String::new();
    let path = |o: &mut String, p: &[[f32; 2]]| {
        if pdf {
            for (i, q) in p.iter().enumerate() {
                let _ = write!(o, "{:.2} {:.2} {} ", q[0], y(q[1]), if i == 0 { "m" } else { "l" });
            }
            o.push_str("h ");
        } else {
            o.push_str("newpath ");
            for (i, q) in p.iter().enumerate() {
                let _ = write!(o, "{:.2} {:.2} {} ", q[0], y(q[1]), if i == 0 { "moveto" } else { "lineto" });
            }
            o.push_str("closepath ");
        }
    };
    let set_fill = |o: &mut String, c: [u8; 3]| {
        let _ = write!(o, "{} {} ", rgb01(c), if pdf { "rg" } else { "setrgbcolor" });
    };
    let set_stroke = |o: &mut String, c: [u8; 3], w: f32| {
        if pdf {
            let _ = write!(o, "{} RG {w:.2} w ", rgb01(c));
        } else {
            let _ = write!(o, "{} setrgbcolor {w:.2} setlinewidth ", rgb01(c));
        }
    };
    if !s.transparent {
        set_fill(&mut o, [255, 255, 255]);
        path(&mut o, &[[0.0, 0.0], [s.w, 0.0], [s.w, s.h], [0.0, s.h]]);
        o.push_str(if pdf { "f\n" } else { "fill\n" });
    }
    let fill_op = if pdf { "f" } else { "fill" };
    let stroke_op = if pdf { "S" } else { "stroke" };
    let line = |o: &mut String, a: [f32; 2], b: [f32; 2], col: [u8; 3], w: f32| {
        set_stroke(o, col, w);
        path(o, &[a, b]);
        if pdf {
            let _ = writeln!(o, "S");
        } else {
            let _ = writeln!(o, "stroke");
        }
    };
    for it in items {
        match it {
            V::Poly { pts, fill, stroke } => {
                set_fill(&mut o, *fill);
                if let Some((c, w)) = stroke {
                    set_stroke(&mut o, *c, *w);
                    path(&mut o, pts);
                    if pdf {
                        o.push_str("B\n");
                    } else {
                        // fill then stroke the same path
                        o.push_str("gsave fill grestore stroke\n");
                    }
                } else {
                    path(&mut o, pts);
                    let _ = writeln!(o, "{fill_op}");
                }
            }
            V::Outline { pts, col, width } => {
                set_stroke(&mut o, *col, *width);
                path(&mut o, pts);
                let _ = writeln!(o, "{stroke_op}");
            }
            V::Line { a, b, col, width } => line(&mut o, *a, *b, *col, *width),
            V::Overlay(Prim::Line { a, b, col, width }) => line(&mut o, *a, *b, *col, *width),
            V::Overlay(Prim::Rect { x, y: ry, w, h: rh, col }) => {
                set_fill(&mut o, *col);
                path(&mut o, &[[*x, *ry], [x + w, *ry], [x + w, ry + rh], [*x, ry + rh]]);
                let _ = writeln!(o, "{fill_op}");
            }
            V::Overlay(Prim::Text { x, y: ty, size, col, text }) => {
                let base = y(ty + size * 0.82);
                set_fill(&mut o, *col);
                if pdf {
                    let _ = writeln!(o, "BT /F1 {size:.2} Tf {x:.2} {base:.2} Td ({}) Tj ET", ps_escape(text));
                } else {
                    let _ = writeln!(
                        o,
                        "/Helvetica findfont {size:.2} scalefont setfont {x:.2} {base:.2} moveto ({}) show",
                        ps_escape(text)
                    );
                }
            }
        }
    }
    o
}

fn write_pdf(s: &VectorScene, items: &[V]) -> Result<Vec<u8>> {
    let content = paint(s, items, true);
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(content.as_bytes())?;
    let comp = z.finish()?;
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offs = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offs.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(&mut out, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    obj(
        &mut out,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
            s.w, s.h
        )
        .as_bytes(),
    );
    let mut body = format!("<< /Length {} /Filter /FlateDecode >>\nstream\n", comp.len()).into_bytes();
    body.extend_from_slice(&comp);
    body.extend_from_slice(b"\nendstream");
    obj(&mut out, &body);
    obj(&mut out, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offs.len() + 1).as_bytes());
    for o in &offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offs.len() + 1).as_bytes(),
    );
    Ok(out)
}

fn write_ps(s: &VectorScene, items: &[V], eps: bool) -> String {
    let mut o = String::new();
    if eps {
        o.push_str("%!PS-Adobe-3.0 EPSF-3.0\n");
    } else {
        o.push_str("%!PS-Adobe-3.0\n");
    }
    let _ = writeln!(o, "%%BoundingBox: 0 0 {} {}", s.w.ceil(), s.h.ceil());
    let _ = writeln!(o, "%%HiResBoundingBox: 0 0 {:.2} {:.2}", s.w, s.h);
    o.push_str("%%EndComments\n");
    o.push_str(&paint(s, items, false));
    if !eps {
        o.push_str("showpage\n");
    }
    o.push_str("%%EOF\n");
    o
}

pub fn export(path: &std::path::Path, fmt: &str, scene: &VectorScene) -> Result<()> {
    let items = build(scene);
    match fmt {
        "svg" => std::fs::write(path, svg(scene, &items))?,
        "pdf" => std::fs::write(path, write_pdf(scene, &items)?)?,
        "eps" => std::fs::write(path, write_ps(scene, &items, true))?,
        "ps" => std::fs::write(path, write_ps(scene, &items, false))?,
        other => bail!("unknown vector format '{other}'"),
    }
    Ok(())
}

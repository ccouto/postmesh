//! Readers for GiD ASCII postprocess files (`.msh` meshes, `.res` results).

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CellKind {
    Line,
    Tri,
    Quad,
    Tet,
    Hex,
    Prism,
    Pyramid,
}

impl CellKind {
    fn from_name(name: &str) -> Option<(CellKind, usize)> {
        Some(match name.to_ascii_lowercase().as_str() {
            "linear" => (CellKind::Line, 2),
            "triangle" => (CellKind::Tri, 3),
            "quadrilateral" => (CellKind::Quad, 4),
            "tetrahedra" => (CellKind::Tet, 4),
            "hexahedra" => (CellKind::Hex, 8),
            "prism" => (CellKind::Prism, 6),
            "pyramid" => (CellKind::Pyramid, 5),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub kind: CellKind,
    /// Node *indices* (into `Mesh::coords`); only the first `nverts` are valid.
    pub n: [u32; 8],
}

pub struct Mesh {
    pub node_ids: Vec<i64>,
    pub coords: Vec<[f64; 3]>,
    pub cells: Vec<Cell>,
    pub node_index: HashMap<i64, u32>,
}

/// Split a line into tokens, honouring double quotes.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut had = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                in_q = !in_q;
                had = true;
            }
            c if c.is_whitespace() && !in_q => {
                if had || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    had = false;
                }
            }
            c => cur.push(c),
        }
    }
    if had || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_data_line(t: &str) -> bool {
    matches!(t.as_bytes().first(), Some(b'0'..=b'9' | b'-' | b'+' | b'.'))
}

pub fn read_mesh(path: &Path) -> Result<Mesh> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut rd = BufReader::with_capacity(1 << 20, f);
    let mut node_ids = Vec::new();
    let mut coords: Vec<[f64; 3]> = Vec::new();
    let mut raw_cells: Vec<(CellKind, [i64; 8])> = Vec::new();
    let mut cur: Option<(CellKind, usize)> = None;
    let mut dim = 3usize;
    // 0 = none, 1 = coordinates, 2 = elements
    let mut section = 0u8;
    let mut line = String::new();
    loop {
        line.clear();
        if rd.read_line(&mut line)? == 0 {
            break;
        }
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if section != 0 && is_data_line(t) {
            let mut it = t.split_ascii_whitespace();
            let id: i64 = it.next().unwrap().parse().context("bad id")?;
            if section == 1 {
                let mut c = [0.0f64; 3];
                for item in c.iter_mut().take(dim) {
                    *item = it.next().context("short coordinate line")?.parse()?;
                }
                node_ids.push(id);
                coords.push(c);
            } else if let Some((kind, nn)) = cur {
                let mut n = [0i64; 8];
                for item in n.iter_mut().take(nn.min(8)) {
                    *item = it.next().context("short element line")?.parse()?;
                }
                raw_cells.push((kind, n));
            }
            continue;
        }
        let low = t.to_ascii_lowercase();
        if low.starts_with("mesh") {
            let tk = tokens(t);
            let lt: Vec<String> = tk.iter().map(|s| s.to_ascii_lowercase()).collect();
            let find = |k: &str| lt.iter().position(|s| s == k).and_then(|i| tk.get(i + 1));
            if let Some(d) = find("dimension") {
                dim = d.parse().unwrap_or(3).clamp(1, 3);
            }
            let et = find("elemtype").context("MESH without ElemType")?;
            let (kind, mut nn) = CellKind::from_name(et)
                .with_context(|| format!("unsupported ElemType '{et}'"))?;
            if let Some(n) = find("nnode") {
                nn = n.parse().unwrap_or(nn);
            }
            cur = Some((kind, nn));
            section = 0;
        } else if low == "coordinates" {
            section = 1;
        } else if low == "elements" {
            section = 2;
        } else if low.starts_with("end") {
            section = 0;
        }
    }
    if node_ids.is_empty() {
        bail!("no coordinates found in mesh file");
    }
    let node_index: HashMap<i64, u32> =
        node_ids.iter().enumerate().map(|(i, &n)| (n, i as u32)).collect();
    let mut cells = Vec::with_capacity(raw_cells.len());
    'cells: for (kind, ids) in raw_cells {
        let mut n = [0u32; 8];
        for (k, id) in ids.iter().enumerate() {
            if k >= nverts(kind) {
                break;
            }
            match node_index.get(id) {
                Some(&i) => n[k] = i,
                None => continue 'cells,
            }
        }
        cells.push(Cell { kind, n });
    }
    Ok(Mesh { node_ids, coords, cells, node_index })
}

pub fn nverts(kind: CellKind) -> usize {
    match kind {
        CellKind::Line => 2,
        CellKind::Tri => 3,
        CellKind::Quad | CellKind::Tet => 4,
        CellKind::Pyramid => 5,
        CellKind::Prism => 6,
        CellKind::Hex => 8,
    }
}

pub struct ResultBlock {
    pub name: String,
    pub step: f64,
    pub components: Vec<String>,
    pub ncomp: usize,
    pub ids: Vec<i64>,
    pub values: Vec<f32>,
}

pub fn read_results(path: &Path) -> Result<Vec<ResultBlock>> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut rd = BufReader::with_capacity(1 << 20, f);
    let mut out = Vec::new();
    let mut hdr: Option<(String, f64, bool)> = None; // name, step, supported
    let mut comps: Vec<String> = Vec::new();
    let mut in_values = false;
    let mut ids = Vec::new();
    let mut vals: Vec<f32> = Vec::new();
    let mut ncomp = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        if rd.read_line(&mut line)? == 0 {
            break;
        }
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if in_values {
            if t.len() >= 3 && t[..3].eq_ignore_ascii_case("end") {
                if let Some((name, step, true)) = hdr.take() {
                    if ncomp > 0 {
                        let components = if comps.len() == ncomp {
                            comps.clone()
                        } else if ncomp == 1 {
                            vec![name.clone()]
                        } else {
                            (1..=ncomp).map(|i| format!("C{i}")).collect()
                        };
                        out.push(ResultBlock {
                            name,
                            step,
                            components,
                            ncomp,
                            ids: std::mem::take(&mut ids),
                            values: std::mem::take(&mut vals),
                        });
                    }
                }
                ids.clear();
                vals.clear();
                ncomp = 0;
                in_values = false;
            } else if hdr.as_ref().is_some_and(|h| h.2) {
                let mut it = t.split_ascii_whitespace();
                let id: i64 = it.next().unwrap().parse().context("bad node id in results")?;
                let start = vals.len();
                for v in it {
                    vals.push(v.parse().context("bad value in results")?);
                }
                let n = vals.len() - start;
                if ncomp == 0 {
                    ncomp = n;
                }
                if n != ncomp {
                    bail!("inconsistent number of components in results");
                }
                ids.push(id);
            }
            continue;
        }
        let low = t.to_ascii_lowercase();
        if low.starts_with("result ") || low.starts_with("result\t") {
            let tk = tokens(t);
            if tk.len() >= 5 {
                let supported = tk.get(5).is_some_and(|s| s.eq_ignore_ascii_case("onnodes"));
                hdr = Some((tk[1].clone(), tk[3].parse().unwrap_or(0.0), supported));
            }
            comps.clear();
        } else if low.starts_with("componentnames") {
            comps = tokens(t).into_iter().skip(1).collect();
        } else if low == "values" && hdr.is_some() {
            in_values = true;
        }
    }
    Ok(out)
}

/// Given a `.msh` or `.res` path return the (mesh, results) pair that exists on disk.
pub fn sibling_files(path: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let base = name
        .strip_suffix(".msh")
        .or_else(|| name.strip_suffix(".res"))
        .map(str::to_string);
    let Some(base) = base else { return (None, None) };
    let msh = path.with_file_name(format!("{base}.msh"));
    let res = path.with_file_name(format!("{base}.res"));
    (msh.exists().then_some(msh), res.exists().then_some(res))
}

/// C-style `%g` formatting with `sig` significant digits.
pub fn fmt_g(v: f64, sig: usize) -> String {
    if v == 0.0 || !v.is_finite() {
        return if v == 0.0 { "0".into() } else { format!("{v}") };
    }
    let exp = v.abs().log10().floor() as i32;
    let s = if exp < -4 || exp >= sig as i32 {
        let m = format!("{:.*e}", sig.saturating_sub(1), v);
        // Rust gives "1.234e-7"; convert to "1.234e-07" and trim zeros.
        let (mant, ex) = m.split_once('e').unwrap();
        let mant = trim_zeros(mant);
        let ex: i32 = ex.parse().unwrap();
        format!("{mant}e{}{:02}", if ex < 0 { '-' } else { '+' }, ex.abs())
    } else {
        let dec = (sig as i32 - 1 - exp).max(0) as usize;
        trim_zeros(&format!("{v:.dec$}"))
    };
    s
}

fn trim_zeros(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// The file that defines the model geometry for any supported input
/// (`.frd` is its own model; `.msh`/`.res` resolve to the `.msh`).
pub fn model_path(path: &Path) -> Option<PathBuf> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("frd")) {
        return path.exists().then(|| path.to_path_buf());
    }
    sibling_files(path).0
}

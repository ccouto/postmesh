//! Reader for CalculiX `.frd` result files (CGX manual, "Result Format").
//!
//! Handles
//!  * nodal point coordinate blocks (`2C`) and element definition blocks (`3C`)
//!  * nodal result blocks (`100C`, `-4` dataset, `-5` entities, `-6`, `-1`/`-2` data, `-3` end)
//!  * the three data encodings: FORMAT 0 (short ASCII), 1 (long ASCII) and 2 (binary,
//!    little-endian 32-bit int/float as written by CalculiX)
//!  * entities flagged "calculated" (IEXIST = 1, e.g. `ALL`) that carry no data columns
//!  * all CalculiX element types (1..=12); quadratic elements are reduced to their corner nodes
//!  * ignored: header records (1C/1U/1P...), material-dependent and element-at-node datasets

use crate::gidio::{Cell, CellKind, Mesh, ResultBlock};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::Path;

struct Rd<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn line(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.d.len() {
            return None;
        }
        let rest = &self.d[self.pos..];
        let (len, adv) = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => (i, i + 1),
            None => (rest.len(), rest.len()),
        };
        self.pos += adv;
        let mut l = &rest[..len];
        if l.last() == Some(&b'\r') {
            l = &l[..l.len() - 1];
        }
        Some(l)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.d.len() {
            bail!("unexpected end of file in binary block");
        }
        let s = &self.d[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// Binary blocks may be followed by a line break.
    fn skip_newline(&mut self) {
        if self.d.get(self.pos) == Some(&b'\r') {
            self.pos += 1;
        }
        if self.d.get(self.pos) == Some(&b'\n') {
            self.pos += 1;
        }
    }
}

fn field(l: &[u8], a: usize, b: usize) -> &[u8] {
    let a = a.min(l.len());
    let b = b.min(l.len());
    &l[a..b]
}

fn trim(s: &[u8]) -> &[u8] {
    let a = s.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(s.len());
    let b = s.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(a, |i| i + 1);
    &s[a..b]
}

fn text(s: &[u8]) -> String {
    String::from_utf8_lossy(trim(s)).into_owned()
}

fn int(s: &[u8]) -> Option<i64> {
    std::str::from_utf8(trim(s)).ok()?.parse().ok()
}

/// Fortran E12.5 field. Copes with run-together fields, 3-digit exponents written without
/// the `E` (e.g. `1.00000-100`) and non-finite values (mapped to 0).
fn num(s: &[u8]) -> f64 {
    let t = trim(s);
    let Ok(st) = std::str::from_utf8(t) else { return 0.0 };
    let v = match st.parse::<f64>() {
        Ok(v) => v,
        Err(_) => {
            let b = st.as_bytes();
            match (1..b.len()).rev().find(|&i| (b[i] == b'+' || b[i] == b'-') && b[i - 1] != b'e' && b[i - 1] != b'E') {
                Some(i) => format!("{}E{}", &st[..i], &st[i..]).parse().unwrap_or(0.0),
                None => 0.0,
            }
        }
    };
    if v.is_finite() { v } else { 0.0 }
}

/// Element type -> (cell kind, corner nodes used for display, total nodes in the record)
fn elem_type(t: i64) -> Option<(CellKind, usize, usize)> {
    Some(match t {
        1 => (CellKind::Hex, 8, 8),
        2 => (CellKind::Prism, 6, 6),
        3 => (CellKind::Tet, 4, 4),
        4 => (CellKind::Hex, 8, 20),
        5 => (CellKind::Prism, 6, 15),
        6 => (CellKind::Tet, 4, 10),
        7 => (CellKind::Tri, 3, 3),
        8 => (CellKind::Tri, 3, 6),
        9 => (CellKind::Quad, 4, 4),
        10 => (CellKind::Quad, 4, 8),
        11 => (CellKind::Line, 2, 2),
        12 => (CellKind::Line, 2, 3),
        _ => return None,
    })
}

fn key(l: &[u8]) -> &[u8] {
    trim(field(l, 1, 3))
}

#[derive(Default)]
struct RawMesh {
    node_ids: Vec<i64>,
    coords: Vec<[f64; 3]>,
    /// (element type, node ids)
    elems: Vec<(i64, Vec<i64>)>,
}

fn read_nodes(rd: &mut Rd, header: &[u8], raw: &mut RawMesh) -> Result<()> {
    let n = int(field(header, 24, 36)).unwrap_or(0).max(0) as usize;
    let fmt = header.get(73).map_or(0, |b| (*b as char).to_digit(10).unwrap_or(0));
    if fmt == 2 {
        for _ in 0..n {
            let id = rd.i32()? as i64;
            let c = [rd.f32()? as f64, rd.f32()? as f64, rd.f32()? as f64];
            raw.node_ids.push(id);
            raw.coords.push(c);
        }
        rd.skip_newline();
        return Ok(());
    }
    let w = if fmt == 1 { 10 } else { 5 };
    loop {
        let save = rd.pos;
        let Some(l) = rd.line() else { break };
        match key(l) {
            b"-1" => {
                let id = int(field(l, 3, 3 + w)).context("bad node number")?;
                let o = 3 + w;
                raw.node_ids.push(id);
                raw.coords.push([num(field(l, o, o + 12)), num(field(l, o + 12, o + 24)), num(field(l, o + 24, o + 36))]);
            }
            b"-3" => break,
            b"" => {}
            _ => {
                rd.pos = save;
                break;
            }
        }
    }
    Ok(())
}

fn read_elements(rd: &mut Rd, header: &[u8], raw: &mut RawMesh) -> Result<()> {
    let n = int(field(header, 24, 36)).unwrap_or(0).max(0) as usize;
    let fmt = header.get(73).map_or(0, |b| (*b as char).to_digit(10).unwrap_or(0));
    if fmt == 2 {
        for _ in 0..n {
            let _id = rd.i32()?;
            let ty = rd.i32()? as i64;
            let _grp = rd.i32()?;
            let _mat = rd.i32()?;
            let Some((_, _, nn)) = elem_type(ty) else { bail!("binary element block contains unknown element type {ty}") };
            let mut nodes = Vec::with_capacity(nn);
            for _ in 0..nn {
                nodes.push(rd.i32()? as i64);
            }
            raw.elems.push((ty, nodes));
        }
        rd.skip_newline();
        return Ok(());
    }
    let w = if fmt == 1 { 10 } else { 5 };
    let mut cur: Option<(i64, Vec<i64>)> = None;
    loop {
        let save = rd.pos;
        let Some(l) = rd.line() else { break };
        match key(l) {
            b"-1" => {
                if let Some(e) = cur.take() {
                    raw.elems.push(e);
                }
                let o = 3 + w;
                let ty = int(field(l, o, o + 5)).context("bad element type")?;
                cur = Some((ty, Vec::new()));
            }
            b"-2" => {
                if let Some((_, nodes)) = cur.as_mut() {
                    let mut o = 3;
                    while o < l.len() {
                        if let Some(v) = int(field(l, o, o + w)) {
                            nodes.push(v);
                        }
                        o += w;
                    }
                }
            }
            b"-3" => break,
            b"" => {}
            _ => {
                rd.pos = save;
                break;
            }
        }
    }
    if let Some(e) = cur.take() {
        raw.elems.push(e);
    }
    Ok(())
}

struct Entity {
    name: String,
    iexist: i64,
}

fn read_result(rd: &mut Rd, header: &[u8], out: &mut Vec<ResultBlock>) -> Result<()> {
    let value = num(field(header, 12, 24));
    let n = int(field(header, 24, 36)).unwrap_or(0).max(0) as usize;
    let fmt = int(field(header, 73, 75)).unwrap_or(0);
    // dataset header
    let mut dataset = None;
    let mut entities: Vec<Entity> = Vec::new();
    let (mut name, mut ncomps, mut irtype) = (String::new(), 0usize, 1i64);
    loop {
        let save = rd.pos;
        let Some(l) = rd.line() else { break };
        match key(l) {
            b"-4" => {
                name = text(field(l, 5, 13));
                ncomps = int(field(l, 13, 18)).unwrap_or(0).max(0) as usize;
                irtype = int(field(l, 18, 23)).unwrap_or(1);
                dataset = Some(());
            }
            b"-5" => entities.push(Entity { name: text(field(l, 5, 13)), iexist: int(field(l, 33, 38)).unwrap_or(0) }),
            b"-6" | b"" => {}
            _ => {
                rd.pos = save;
                break;
            }
        }
    }
    if dataset.is_none() {
        return Ok(());
    }
    let _ = ncomps;
    let cols: Vec<String> = entities.iter().filter(|e| e.iexist != 1).map(|e| e.name.clone()).collect();
    let nc = cols.len();
    let supported = irtype == 1 && nc > 0;
    let mut ids = Vec::with_capacity(n);
    let mut vals: Vec<f32> = Vec::with_capacity(n * nc);

    if fmt == 2 {
        if !supported {
            bail!("binary result block '{name}' of an unsupported dataset type");
        }
        for _ in 0..n {
            ids.push(rd.i32()? as i64);
            for _ in 0..nc {
                vals.push(rd.f32()?);
            }
        }
        rd.skip_newline();
    } else {
        let w = if fmt == 1 { 10 } else { 5 };
        let o = 3 + w;
        loop {
            let Some(l) = rd.line() else { break };
            match key(l) {
                b"-1" if supported => {
                    ids.push(int(field(l, 3, 3 + w)).context("bad node number in results")?);
                    let start = vals.len();
                    let take = |l: &[u8], vals: &mut Vec<f32>| {
                        let mut k = 0;
                        while vals.len() - start < nc && k < 6 {
                            vals.push(num(field(l, o + 12 * k, o + 12 * (k + 1))) as f32);
                            k += 1;
                        }
                    };
                    take(l, &mut vals);
                    while vals.len() - start < nc {
                        let save = rd.pos;
                        match rd.line() {
                            Some(c) if key(c) == b"-2" => take(c, &mut vals),
                            _ => {
                                rd.pos = save;
                                break;
                            }
                        }
                    }
                    vals.resize(start + nc, 0.0);
                }
                b"-3" => break,
                _ => {}
            }
        }
    }
    if supported && !ids.is_empty() {
        out.push(ResultBlock { name, step: value, components: cols, ncomp: nc, ids, values: vals });
    }
    Ok(())
}

pub struct FrdContent {
    pub mesh: Mesh,
    pub results: Vec<ResultBlock>,
    /// elements that could not be displayed (unknown type, missing nodes)
    pub skipped_elements: usize,
}

pub fn read_frd(path: &Path) -> Result<FrdContent> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    parse_frd(&data)
}

pub fn parse_frd(data: &[u8]) -> Result<FrdContent> {
    let mut rd = Rd { d: data, pos: 0 };
    let mut raw = RawMesh::default();
    let mut results = Vec::new();
    while let Some(l) = rd.line() {
        if l.len() < 6 {
            if trim(l) == b"9999" {
                break;
            }
            continue;
        }
        let code = l[5];
        match (trim(field(l, 1, 5)), code) {
            (b"2", b'C') => read_nodes(&mut rd, l, &mut raw)?,
            (b"3", b'C') => read_elements(&mut rd, l, &mut raw)?,
            (b"100", b'C') => read_result(&mut rd, l, &mut results)?,
            _ => {}
        }
    }
    if raw.node_ids.is_empty() {
        bail!("no node block found: is this a CalculiX .frd file?");
    }
    let node_index: HashMap<i64, u32> = raw.node_ids.iter().enumerate().map(|(i, &n)| (n, i as u32)).collect();
    let mut cells = Vec::with_capacity(raw.elems.len());
    let mut skipped = 0;
    for (ty, nodes) in &raw.elems {
        let Some((kind, ncorner, _)) = elem_type(*ty) else {
            skipped += 1;
            continue;
        };
        let idx: Vec<Option<u32>> = nodes.iter().map(|n| node_index.get(n).copied()).collect();
        let mut n = [0u32; 8];
        let ok = if *ty == 12 {
            // 3-node beam: use the two end nodes, whichever order the file lists them in
            if idx.len() >= 3 && idx.iter().take(3).all(|i| i.is_some()) {
                let p: Vec<[f64; 3]> = idx.iter().take(3).map(|i| raw.coords[i.unwrap() as usize]).collect();
                let d = |a: usize, b: usize| (0..3).map(|k| (p[a][k] - p[b][k]).powi(2)).sum::<f64>();
                let (a, b) = [(0, 1), (0, 2), (1, 2)].into_iter().max_by(|x, y| d(x.0, x.1).total_cmp(&d(y.0, y.1))).unwrap();
                n[0] = idx[a].unwrap();
                n[1] = idx[b].unwrap();
                true
            } else {
                false
            }
        } else if idx.len() >= ncorner && idx.iter().take(ncorner).all(|i| i.is_some()) {
            for k in 0..ncorner {
                n[k] = idx[k].unwrap();
            }
            true
        } else {
            false
        };
        if ok {
            cells.push(Cell { kind, n });
        } else {
            skipped += 1;
        }
    }
    let mesh = Mesh { node_ids: raw.node_ids, coords: raw.coords, cells, node_index };
    Ok(FrdContent { mesh, results, skipped_elements: skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(key: &str, n: usize, fmt: u8) -> String {
        format!(" {:>4}C{:18}{:>12}{:37}{}\n", key, "", n, "", fmt)
    }

    #[test]
    fn long_ascii_nodes_elements_results() {
        let mut s = String::from("    1C\n");
        s += &header("2", 8, 1);
        for (i, c) in [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.], [0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]].iter().enumerate() {
            s += &format!(" -1{:>10}{:12.5E}{:12.5E}{:12.5E}\n", 10 + i, c[0], c[1], c[2]);
        }
        s += " -3\n";
        s += &header("3", 1, 1);
        s += &format!(" -1{:>10}{:>5}{:>5}{:>5}\n", 1, 1, 0, 1);
        s += &format!(" -2{}\n", (10..18).map(|i| format!("{i:>10}")).collect::<String>());
        s += " -3\n";
        s += "    1PSTEP                         1           1           1\n";
        s += &format!("  100CL  101{:>12.5E}{:>12}{:20}{:>2}{:>5}{:10}{:>2}\n", 2.5, 8, "", 0, 1, "", 1);
        s += " -4  STRESS      6    1\n";
        for (i, n) in ["SXX", "SYY", "SZZ", "SXY", "SYZ", "SZX"].iter().enumerate() {
            s += &format!(" -5  {:<8}{:>5}{:>5}{:>5}{:>5}\n", n, 1, 4, 1 + i % 3, 1 + i / 3);
        }
        for i in 0..8 {
            s += &format!(" -1{:>10}", 10 + i);
            for k in 0..6 {
                s += &format!("{:12.5E}", (i * 10 + k) as f64);
            }
            s += "\n";
        }
        s += " -3\n 9999\n";
        let f = parse_frd(s.as_bytes()).unwrap();
        assert_eq!(f.mesh.coords.len(), 8);
        assert_eq!(f.mesh.cells.len(), 1);
        assert_eq!(f.results.len(), 1);
        let r = &f.results[0];
        assert_eq!((r.name.as_str(), r.ncomp, r.step), ("STRESS", 6, 2.5));
        assert_eq!(r.components[3], "SXY");
        assert_eq!(r.values[6 * 3 + 2], 32.0);
    }

    #[test]
    fn binary_and_calculated_entities() {
        let mut b: Vec<u8> = Vec::new();
        b.extend(header("2", 4, 2).bytes());
        for (i, c) in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]].iter().enumerate() {
            b.extend((5 + i as i32).to_le_bytes());
            for v in c {
                b.extend(v.to_le_bytes());
            }
        }
        b.push(b'\n');
        b.extend(header("3", 1, 2).bytes());
        for v in [1i32, 3, 0, 1, 5, 6, 7, 8] {
            b.extend(v.to_le_bytes());
        }
        b.push(b'\n');
        b.extend(format!("  100CL  101{:>12.5E}{:>12}{:20}{:>2}{:>5}{:10}{:>2}\n", 1.0, 4, "", 0, 1, "", 2).bytes());
        b.extend(" -4  DISP        4    1\n".bytes());
        b.extend(format!(" -5  {:<8}{:>5}{:>5}{:>5}{:>5}\n -5  {:<8}{:>5}{:>5}{:>5}{:>5}\n -5  {:<8}{:>5}{:>5}{:>5}{:>5}\n", "D1", 1, 2, 1, 0, "D2", 1, 2, 2, 0, "D3", 1, 2, 3, 0).bytes());
        b.extend(format!(" -5  {:<8}{:>5}{:>5}{:>5}{:>5}{:>5}ALL\n", "ALL", 1, 2, 0, 0, 1).bytes());
        for i in 0..4 {
            b.extend((5 + i as i32).to_le_bytes());
            for k in 0..3 {
                b.extend(((i * 3 + k) as f32).to_le_bytes());
            }
        }
        b.push(b'\n');
        b.extend(b" 9999\n");
        let f = parse_frd(&b).unwrap();
        assert_eq!(f.mesh.cells.len(), 1);
        assert_eq!(f.results[0].components, vec!["D1", "D2", "D3"]);
        assert_eq!(f.results[0].values[11], 11.0);
    }

    #[test]
    fn fortran_numbers() {
        assert_eq!(num(b" 1.50000E+02"), 150.0);
        assert_eq!(num(b"-2.00000E-03"), -0.002);
        assert!(num(b" 1.00000-100") < 1e-90);
        assert_eq!(num(b"************"), 0.0);
    }
}

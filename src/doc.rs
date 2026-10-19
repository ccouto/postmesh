//! A loaded model (mesh + results), its extracted surface, and the per-view scene data.

use crate::gidio::{self, Cell, CellKind, Mesh, fmt_g};
use crate::view::ViewOptions;
use anyhow::{Result, bail};
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const NONE_LABEL: &str = "(none)";
pub const MAGNITUDE: &str = "Magnitude";
const NO_NODE: u32 = u32::MAX;

pub struct ResultStep {
    pub step: f64,
    /// `nnodes * ncomp` values in mesh node order.
    pub values: Vec<f32>,
}

pub struct ResultSet {
    pub name: String,
    pub components: Vec<String>,
    pub steps: Vec<ResultStep>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Scalar,
    Vector,
    /// symmetric tensor stored as XX YY ZZ XY YZ ZX (CalculiX stress / strain)
    Tensor,
}

const DERIVED_TENSOR: [&str; 3] = ["Max principal", "Mid principal", "Min principal"];

impl ResultSet {
    pub fn ncomp(&self) -> usize {
        self.components.len()
    }

    pub fn kind(&self) -> Kind {
        let up: Vec<String> = self.components.iter().map(|c| c.to_ascii_uppercase()).collect();
        let sfx = ["XX", "YY", "ZZ", "XY", "YZ", "ZX"];
        if up.len() == 6 && up.iter().zip(sfx).all(|(c, s)| c.ends_with(s) || (s == "ZX" && c.ends_with("XZ"))) {
            Kind::Tensor
        } else if up.len() == 1 {
            Kind::Scalar
        } else {
            Kind::Vector
        }
    }

    fn is_stress(&self) -> bool {
        self.components.first().is_some_and(|c| c.to_ascii_uppercase().starts_with('S'))
    }

    /// Can this result drive the deformation (>= 3 components, not a tensor)?
    pub fn is_deform_source(&self) -> bool {
        self.ncomp() >= 3 && self.kind() != Kind::Tensor
    }

    /// Selectable contour components, first entry is the default.
    pub fn choices(&self) -> Vec<String> {
        match self.kind() {
            Kind::Scalar => self.components.clone(),
            Kind::Vector => std::iter::once(MAGNITUDE.to_string()).chain(self.components.iter().cloned()).collect(),
            Kind::Tensor => {
                let eq = if self.is_stress() { ["Von Mises", "Tresca"] } else { ["Equivalent", "Max shear"] };
                eq.iter().map(|s| s.to_string()).chain(DERIVED_TENSOR.iter().map(|s| s.to_string())).chain(self.components.iter().cloned()).collect()
            }
        }
    }

    /// Per-node scalar for one component choice at a step.
    pub fn scalar(&self, comp: &str, step: usize, nn: usize) -> Option<Vec<f32>> {
        let st = self.steps.get(step)?;
        let nc = self.ncomp();
        let row = |i: usize| &st.values[i * nc..(i + 1) * nc];
        if let Some(k) = self.components.iter().position(|c| c == comp) {
            return Some((0..nn).map(|i| row(i)[k]).collect());
        }
        match self.kind() {
            Kind::Vector if comp == MAGNITUDE => Some((0..nn).map(|i| row(i).iter().map(|x| x * x).sum::<f32>().sqrt()).collect()),
            Kind::Tensor => {
                let stress = self.is_stress();
                let f = |i: usize| -> Option<f32> {
                    let t = row(i);
                    let t: [f64; 6] = std::array::from_fn(|k| t[k] as f64);
                    let [xx, yy, zz, xy, yz, zx] = t;
                    let (p1, p2, p3) = principal(t);
                    Some(match comp {
                        "Von Mises" => (0.5 * ((xx - yy).powi(2) + (yy - zz).powi(2) + (zz - xx).powi(2)) + 3.0 * (xy * xy + yz * yz + zx * zx)).sqrt() as f32,
                        "Equivalent" => ((2.0 / 3.0) * (xx * xx + yy * yy + zz * zz + 2.0 * (xy * xy + yz * yz + zx * zx))).sqrt() as f32,
                        "Tresca" | "Max shear" => (p1 - p3) as f32,
                        "Max principal" => p1 as f32,
                        "Mid principal" => p2 as f32,
                        "Min principal" => p3 as f32,
                        _ => return None,
                    })
                };
                f(0)?;
                let _ = stress;
                Some((0..nn).map(|i| f(i).unwrap_or(0.0)).collect())
            }
            _ => None,
        }
    }
    pub fn step_labels(&self) -> Vec<String> {
        self.steps.iter().map(|s| fmt_g(s.step, 6)).collect()
    }
}

/// Boundary polygons (tri: 4th index is `u32::MAX`) and mesh edges.
pub struct Surface {
    pub polys: Vec<[u32; 4]>,
    /// every unique edge of the surface polygons
    pub edges: Vec<[u32; 2]>,
    /// boundary / sharp edges only (used for the undeformed outline)
    pub feature_edges: Vec<[u32; 2]>,
}

pub struct Document {
    /// elements of the input that could not be displayed
    pub skipped_elements: usize,
    pub msh_path: PathBuf,
    pub mesh: Mesh,
    pub surface: Surface,
    pub results: Vec<ResultSet>,
    pub center: DVec3,
    pub diag: f64,
}

impl Document {
    pub fn load(path: &Path) -> Result<Document> {
        let (msh, mesh, blocks, skipped) = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("frd")) {
            let f = crate::frd::read_frd(path)?;
            (path.to_path_buf(), f.mesh, f.results, f.skipped_elements)
        } else {
            let (msh, res) = gidio::sibling_files(path);
            let Some(msh) = msh else { bail!("could not find the matching .msh file for {}", path.display()) };
            let mesh = gidio::read_mesh(&msh)?;
            let blocks = match res {
                Some(r) => gidio::read_results(&r)?,
                None => Vec::new(),
            };
            (msh, mesh, blocks, 0)
        };
        let nn = mesh.node_ids.len();
        let mut results: Vec<ResultSet> = Vec::new();
        for b in blocks {
            let mut full = vec![0.0f32; nn * b.ncomp];
            for (k, id) in b.ids.iter().enumerate() {
                if let Some(&i) = mesh.node_index.get(id) {
                    let i = i as usize;
                    full[i * b.ncomp..(i + 1) * b.ncomp]
                        .copy_from_slice(&b.values[k * b.ncomp..(k + 1) * b.ncomp]);
                }
            }
            let step = ResultStep { step: b.step, values: full };
            match results.iter_mut().find(|r| r.name == b.name && r.ncomp() == b.ncomp) {
                Some(r) => r.steps.push(step),
                None => results.push(ResultSet { name: b.name, components: b.components, steps: vec![step] }),
            }
        }
        for r in &mut results {
            r.steps.sort_by(|a, b| a.step.total_cmp(&b.step));
            // identical step values (e.g. repeated increments) -> number the steps instead
            if r.steps.windows(2).any(|w| w[0].step == w[1].step) {
                for (i, s) in r.steps.iter_mut().enumerate() {
                    s.step = (i + 1) as f64;
                }
            }
        }
        let surface = build_surface(&mesh);
        let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
        for c in &mesh.coords {
            let p = DVec3::from_array(*c);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let center = (lo + hi) * 0.5;
        let diag = (hi - lo).length().max(1e-12);
        Ok(Document { skipped_elements: skipped, msh_path: msh, mesh, surface, results, center, diag })
    }

    /// Result to deform the shape with by default (prefers one named like a displacement).
    pub fn default_deform(&self) -> Option<String> {
        let mut it = self.results.iter().filter(|r| r.is_deform_source());
        let first = it.next()?;
        let disp = std::iter::once(first).chain(it).find(|r| {
            let n = r.name.to_ascii_lowercase();
            n.contains("disp") || n.contains("deform")
        });
        Some(disp.unwrap_or(first).name.clone())
    }

    pub fn result(&self, name: &str) -> Option<&ResultSet> {
        self.results.iter().find(|r| r.name == name)
    }
}

fn faces_of(c: &Cell) -> Vec<[u32; 4]> {
    let n = &c.n;
    let t = |a: usize, b: usize, c: usize| [n[a], n[b], n[c], NO_NODE];
    let q = |a: usize, b: usize, c: usize, d: usize| [n[a], n[b], n[c], n[d]];
    match c.kind {
        CellKind::Line => vec![],
        CellKind::Tri => vec![t(0, 1, 2)],
        CellKind::Quad => vec![q(0, 1, 2, 3)],
        CellKind::Tet => vec![t(0, 2, 1), t(0, 1, 3), t(1, 2, 3), t(0, 3, 2)],
        CellKind::Hex => vec![
            q(0, 3, 2, 1),
            q(4, 5, 6, 7),
            q(0, 1, 5, 4),
            q(1, 2, 6, 5),
            q(2, 3, 7, 6),
            q(3, 0, 4, 7),
        ],
        CellKind::Prism => vec![t(0, 2, 1), t(3, 4, 5), q(0, 1, 4, 3), q(1, 2, 5, 4), q(2, 0, 3, 5)],
        CellKind::Pyramid => vec![q(0, 3, 2, 1), t(0, 1, 4), t(1, 2, 4), t(2, 3, 4), t(3, 0, 4)],
    }
}

fn sorted_key(f: &[u32; 4]) -> [u32; 4] {
    let mut k = *f;
    k.sort_unstable();
    k
}

fn poly_normal(coords: &[[f64; 3]], f: &[u32; 4]) -> DVec3 {
    let p = |i: usize| DVec3::from_array(coords[f[i] as usize]);
    let n = if f[3] == NO_NODE {
        (p(1) - p(0)).cross(p(2) - p(0))
    } else {
        (p(2) - p(0)).cross(p(3) - p(1))
    };
    n.normalize_or_zero()
}

pub fn poly_len(f: &[u32; 4]) -> usize {
    if f[3] == NO_NODE { 3 } else { 4 }
}

fn build_surface(mesh: &Mesh) -> Surface {
    // Volume faces shared by two cells are interior; keep faces that occur once.
    let mut volume: HashMap<[u32; 4], (u32, [u32; 4])> = HashMap::new();
    let mut polys: Vec<[u32; 4]> = Vec::new();
    let mut line_edges: Vec<[u32; 2]> = Vec::new();
    for c in &mesh.cells {
        match c.kind {
            CellKind::Line => line_edges.push([c.n[0], c.n[1]]),
            CellKind::Tri | CellKind::Quad => polys.extend(faces_of(c)),
            _ => {
                for f in faces_of(c) {
                    volume.entry(sorted_key(&f)).and_modify(|e| e.0 += 1).or_insert((1, f));
                }
            }
        }
    }
    let mut vol: Vec<(u32, [u32; 4])> = volume.into_values().filter(|(n, _)| *n == 1).map(|(n, f)| (n, f)).collect();
    vol.sort_by_key(|(_, f)| *f);
    polys.extend(vol.into_iter().map(|(_, f)| f));

    // edges with adjacent face normals
    let mut emap: HashMap<(u32, u32), (u32, DVec3, DVec3)> = HashMap::new();
    for f in &polys {
        let nrm = poly_normal(&mesh.coords, f);
        let n = poly_len(f);
        for i in 0..n {
            let (a, b) = (f[i], f[(i + 1) % n]);
            let key = if a < b { (a, b) } else { (b, a) };
            let e = emap.entry(key).or_insert((0, nrm, DVec3::ZERO));
            e.0 += 1;
            if e.0 == 2 {
                e.2 = nrm;
            }
        }
    }
    let cos_sharp = 30f64.to_radians().cos();
    let mut edges = Vec::with_capacity(emap.len());
    let mut feature = Vec::new();
    for (&(a, b), &(count, n1, n2)) in &emap {
        edges.push([a, b]);
        if count != 2 || n1.dot(n2).abs() < cos_sharp {
            feature.push([a, b]);
        }
    }
    edges.extend(&line_edges);
    feature.extend(&line_edges);
    edges.sort_unstable();
    feature.sort_unstable();
    Surface { polys, edges, feature_edges: feature }
}

/// Everything derived from the current options that the renderers need.
pub struct SceneData {
    /// deformed positions relative to `Document::center`
    pub positions: Vec<[f32; 3]>,
    pub rest_positions: Vec<[f32; 3]>,
    pub scalars: Vec<f32>,
    pub clim: (f64, f64),
    pub data_range: (f64, f64),
    pub scale: f64,
    pub have_result: bool,
    pub title: String,
    pub caption: Vec<String>,
}

fn norm3(v: &[f32]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// Normalise the selection in `opts` against the document and compute the scene.
/// Mutates `opts` to reflect derived values (auto scale factor, auto range, resolved selection).
pub fn compute_scene(doc: &Document, opts: &mut ViewOptions) -> SceneData {
    let nn = doc.mesh.coords.len();
    let c = doc.center;
    let rest: Vec<[f32; 3]> = doc
        .mesh
        .coords
        .iter()
        .map(|p| [(p[0] - c.x) as f32, (p[1] - c.y) as f32, (p[2] - c.z) as f32])
        .collect();

    // --- resolve result / component / step
    let rs = doc.result(&opts.result).or_else(|| doc.results.first());
    let mut scalars = Vec::new();
    let mut have_result = false;
    let mut title = String::new();
    #[allow(unused_assignments)]
    let mut step_idx = 0usize;
    let mut step_val = 0.0;
    let mut result_name = String::new();
    let mut comp = String::new();
    let mut data_range = (0.0, 0.0);
    if let Some(rs) = rs {
        opts.result = rs.name.clone();
        result_name = rs.name.clone();
        let choices = rs.choices();
        if !choices.contains(&opts.component) {
            opts.component = choices[0].clone();
        }
        let labels = rs.step_labels();
        step_idx = labels.iter().position(|l| *l == opts.step).unwrap_or(labels.len().saturating_sub(1));
        opts.step = labels.get(step_idx).cloned().unwrap_or_default();
        if let Some(st) = rs.steps.get(step_idx) {
            step_val = st.step;
            let _ = st;
            scalars = rs.scalar(&opts.component, step_idx, nn).unwrap_or_else(|| vec![0.0; nn]);
            have_result = true;
            comp = opts.component.clone();
            title = format!("{} ({})", rs.name, comp);
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for &s in &scalars {
                lo = lo.min(s);
                hi = hi.max(s);
            }
            data_range = (lo as f64, hi as f64);
        }
    }
    if !have_result {
        scalars = vec![0.0; nn];
    }

    // --- deformation
    let mut scale = 0.0;
    let mut positions = rest.clone();
    let dname = if opts.deform_with.is_empty() { None } else { Some(opts.deform_with.clone()) };
    if opts.show_deformation {
        if let Some(d) = dname.as_deref().and_then(|n| doc.result(n)).filter(|d| d.is_deform_source()) {
            // the deformation step closest in value to the displayed step
            let di = if have_result {
                (0..d.steps.len()).min_by(|&a, &b| (d.steps[a].step - step_val).abs().total_cmp(&(d.steps[b].step - step_val).abs())).unwrap_or(0)
            } else {
                d.steps.len() - 1
            };
            let st = &d.steps[di];
            let nc = d.ncomp();
            let mut mx = 0.0f32;
            for i in 0..nn {
                mx = mx.max(norm3(&st.values[i * nc..i * nc + 3]));
            }
            let auto = if mx > 0.0 { 0.1 * doc.diag / mx as f64 } else { 1.0 };
            if opts.auto_scale {
                opts.scale_factor = auto;
            }
            scale = opts.scale_factor;
            for i in 0..nn {
                for k in 0..3 {
                    positions[i][k] += (scale * st.values[i * nc + k] as f64) as f32;
                }
            }
        }
    }

    // --- contour range
    let clim = if opts.fixed_range {
        (opts.range_min, opts.range_max)
    } else {
        let (lo, mut hi) = data_range;
        if hi <= lo {
            hi = lo + 1e-30;
        }
        opts.range_min = lo;
        opts.range_max = hi;
        (lo, hi)
    };

    let mut caption = Vec::new();
    if have_result {
        caption.push(format!("step {}", fmt_g(step_val, 6)));
        caption.push(format!("Contour Fill of {}, {}", result_name, comp));
        if scale > 0.0 {
            caption.push(format!("Deformation (x{}): {}", fmt_g(scale, 5), dname.unwrap_or_default()));
        }
    }
    SceneData { positions, rest_positions: rest, scalars, clim, data_range, scale, have_result, title, caption }
}


/// Principal values (descending) of a symmetric tensor [xx, yy, zz, xy, yz, zx].
fn principal(t: [f64; 6]) -> (f64, f64, f64) {
    let [xx, yy, zz, xy, yz, zx] = t;
    let p1 = xy * xy + yz * yz + zx * zx;
    if p1 < 1e-300 {
        let mut d = [xx, yy, zz];
        d.sort_by(|a, b| b.total_cmp(a));
        return (d[0], d[1], d[2]);
    }
    let q = (xx + yy + zz) / 3.0;
    let p2 = (xx - q).powi(2) + (yy - q).powi(2) + (zz - q).powi(2) + 2.0 * p1;
    let p = (p2 / 6.0).sqrt();
    let (a, b, c) = ((xx - q) / p, (yy - q) / p, (zz - q) / p);
    let (d, e, f) = (xy / p, yz / p, zx / p);
    let det = a * (b * c - e * e) - d * (d * c - e * f) + f * (d * e - b * f);
    let r = (det / 2.0).clamp(-1.0, 1.0);
    let phi = r.acos() / 3.0;
    let e1 = q + 2.0 * p * phi.cos();
    let e3 = q + 2.0 * p * (phi + 2.0 * std::f64::consts::PI / 3.0).cos();
    (e1, 3.0 * q - e1 - e3, e3)
}

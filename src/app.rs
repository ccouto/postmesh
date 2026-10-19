//! egui front-end.

use crate::camera::StandardView;
use crate::colormap;
use crate::doc::NONE_LABEL;
use crate::gidio;
use crate::overlay::Prim;
use crate::render::Renderer;
use crate::session::{Session, VECTOR_FORMATS};
use crate::view::{self, ViewOptions};
use eframe::egui::{self, Align2, Color32, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};
use std::path::{Path, PathBuf};

struct ExportDialog {
    format_idx: usize,
    w: u32,
    h: u32,
    aspect: f32,
    transparent: bool,
}

const EXPORT_FORMATS: [(&str, &str); 8] = [
    ("png", "PNG (raster)"),
    ("tiff", "TIFF (raster)"),
    ("jpg", "JPEG (raster)"),
    ("bmp", "BMP (raster)"),
    ("svg", "SVG (vector)"),
    ("pdf", "PDF (vector)"),
    ("eps", "EPS (vector)"),
    ("ps", "PS (vector)"),
];

pub struct App {
    session: Session,
    renderer: Renderer,
    tex_id: Option<egui::TextureId>,
    status: String,
    export: Option<ExportDialog>,
    last_saved: ViewOptions,
    prefs: view::Preferences,
    options_open: bool,
    gv_path: Option<PathBuf>,
    last_gv: Option<ViewOptions>,
    /// modification times of the files the open model was read from
    watch: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    was_focused: bool,
    /// names of files changed on disk, waiting for the user's reload decision
    reload_prompt: Vec<String>,
    about_open: bool,
    gv_warned: bool,
    view_px: (u32, u32),
    error: Option<String>,
    /// developer hook: save a screenshot of the window to this path and exit
    pub gui_screenshot: Option<PathBuf>,
    frames: u32,
    zoom_box: bool,
    box_start: Option<Pos2>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>, view_file: Option<PathBuf>) -> Self {
        let gl = cc.gl.clone().expect("the OpenGL (glow) backend is required");
        let renderer = Renderer::new(&gl).unwrap_or_else(|e| {
            eprintln!("could not initialise OpenGL rendering: {e:#}");
            std::process::exit(1);
        });
        let mut opts = view::load_settings().unwrap_or_default();
        if let Some(vf) = view_file {
            if let Ok(v) = ViewOptions::load(&vf) {
                opts = v;
            }
        }
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let mut app = App {
            session: Session::new(opts.clone()),
            renderer,
            tex_id: None,
            status: "Open a .msh / .res file (File ▸ Open)".into(),
            export: None,
            last_saved: opts,
            prefs: view::Preferences::load(),
            options_open: false,
            gv_path: None,
            last_gv: None,
            watch: Vec::new(),
            was_focused: false,
            reload_prompt: Vec::new(),
            about_open: false,
            gv_warned: false,
            view_px: (1, 1),
            error: None,
            gui_screenshot: None,
            frames: 0,
            zoom_box: false,
            box_start: None,
        };
        if let Some(p) = initial {
            app.open(&p);
        }
        app
    }

    /// Write the current view to the model's `.gv` file (if enabled and something changed).
    fn sync_gv(&mut self, force: bool) {
        let (true, Some(path)) = (self.prefs.save_gv, self.gv_path.clone()) else { return };
        if self.session.doc.is_none() {
            return;
        }
        let snap = self.session.snapshot();
        if !force && self.last_gv.as_ref() == Some(&snap) {
            return;
        }
        match snap.save(&path) {
            Ok(()) => self.last_gv = Some(snap),
            Err(e) => {
                self.last_gv = Some(snap); // do not retry every frame
                if !self.gv_warned {
                    self.gv_warned = true;
                    self.status = format!("Could not write {}: {e}", path.display());
                }
            }
        }
    }

    fn open(&mut self, path: &Path) {
        self.sync_gv(true); // flush the view of the model we are leaving
        // a stored .gv view takes precedence over the options carried over from the last model
        let gv = gidio::model_path(path).map(|m| view::gv_path_for(&m));
        let mut restored = false;
        if let Some(gv) = gv.as_ref().filter(|g| g.exists() && self.prefs.restore_gv) {
            match ViewOptions::load(gv) {
                Ok(v) => {
                    self.session.set_view_for_load(v);
                    restored = true;
                }
                Err(e) => self.status = format!("Ignoring unreadable {}: {e:#}", gv.display()),
            }
        }
        self.gv_path = gv;
        self.last_gv = None;
        self.gv_warned = false;
        match self.session.load(path, &mut self.renderer) {
            Ok(()) => {
                if restored {
                    self.last_gv = Some(self.session.snapshot());
                }
                self.sync_gv(false);
                self.watch = stamps(&self.source_files());
                let d = self.session.doc.as_ref().unwrap();
                self.status = format!(
                    "{}: {} nodes, {} elements, {} result sets",
                    d.msh_path.file_name().unwrap_or_default().to_string_lossy(),
                    d.mesh.coords.len(),
                    d.mesh.cells.len(),
                    d.results.len()
                ) + if restored { " – view restored from .gv" } else { "" }
                    + &if d.skipped_elements > 0 { format!(" – {} elements skipped (unsupported type)", d.skipped_elements) } else { String::new() };
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    /// Files the open model was read from.
    fn source_files(&self) -> Vec<PathBuf> {
        let Some(d) = &self.session.doc else { return Vec::new() };
        let mut v = vec![d.msh_path.clone()];
        if !d.msh_path.extension().is_some_and(|e| e.eq_ignore_ascii_case("frd")) {
            if let Some(r) = gidio::sibling_files(&d.msh_path).1 {
                v.push(r);
            }
        }
        v
    }

    /// Called when the window gains focus: compare file modification times with the ones
    /// recorded when the model was read.
    fn check_files(&mut self) {
        if self.session.doc.is_none() {
            return;
        }
        let now = stamps(&self.source_files());
        let changed: Vec<String> = now
            .iter()
            .filter(|(p, t)| {
                t.is_some() && self.watch.iter().find(|(q, _)| q == p).map(|(_, old)| old) != Some(t)
            })
            .map(|(p, _)| p.file_name().unwrap_or_default().to_string_lossy().to_string())
            .collect();
        if !changed.is_empty() {
            self.reload_prompt = changed;
        }
        self.watch = now; // ask only once per change
    }

    /// Re-read the open model, keeping the current view options and camera.
    fn reload(&mut self) {
        let Some(path) = self.session.doc.as_ref().map(|d| d.msh_path.clone()) else { return };
        let snap = self.session.snapshot();
        self.session.set_view_for_load(snap);
        match self.session.load(&path, &mut self.renderer) {
            Ok(()) => {
                self.watch = stamps(&self.source_files());
                self.status = format!("Reloaded {}", path.file_name().unwrap_or_default().to_string_lossy());
            }
            Err(e) => self.error = Some(format!("Reload failed: {e:#}")),
        }
    }

    fn reload_window(&mut self, ctx: &egui::Context) {
        if self.reload_prompt.is_empty() {
            return;
        }
        let mut action = None;
        egui::Window::new("File changed").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.label(format!("{} changed on disk.", self.reload_prompt.join(", ")));
            ui.label("Reload it? Your current view is kept.");
            ui.horizontal(|ui| {
                if ui.button("Reload").clicked() {
                    action = Some(true);
                }
                if ui.button("Ignore").clicked() {
                    action = Some(false);
                }
            });
        });
        if let Some(reload) = action {
            self.reload_prompt.clear();
            if reload {
                self.reload();
            }
        }
    }

    fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("GiD / CalculiX results", &["msh", "res", "frd"]).pick_file() {
            self.open(&p);
        }
    }

    fn export_view(&mut self) {
        if let Some(p) = rfd::FileDialog::new().set_file_name(format!("{}.json", self.model_stem())).add_filter("View options", &["json"]).save_file() {
            match self.session.snapshot().save(&p) {
                Ok(()) => self.status = format!("Saved view options to {}", p.display()),
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
    }

    fn import_view(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("View options", &["json"]).pick_file() {
            match ViewOptions::load(&p) {
                Ok(v) => {
                    self.session.apply_view(v, &mut self.renderer);
                    self.status = format!("Imported view options from {}", p.display());
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
    }

    /// Name of the open model without its extension (`l5900.frd` -> `l5900`).
    fn model_stem(&self) -> String {
        self.session
            .doc
            .as_ref()
            .and_then(|d| d.msh_path.file_stem().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "view".into())
    }

    fn do_export(&mut self, d: &ExportDialog) {
        let (ext, _) = EXPORT_FORMATS[d.format_idx];
        let stem = self.model_stem();
        let mut dlg = rfd::FileDialog::new().set_file_name(format!("{stem}.{ext}")).add_filter(ext.to_uppercase(), &[ext]);
        if !self.prefs.export_dir.is_empty() && Path::new(&self.prefs.export_dir).is_dir() {
            dlg = dlg.set_directory(&self.prefs.export_dir);
        }
        // remember the choices even if the file dialog is cancelled
        self.prefs.export_format = ext.to_string();
        self.prefs.export_width = d.w;
        self.prefs.export_height = d.h;
        self.prefs.export_transparent = d.transparent;
        self.prefs.save();
        let Some(p) = dlg.save_file() else {
            return;
        };
        if let Some(dir) = p.parent() {
            self.prefs.export_dir = dir.to_string_lossy().to_string();
            self.prefs.save();
        }
        let p = if p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) { p } else { p.with_extension(ext) };
        match self.session.export(&mut self.renderer, &p, ext, d.w, d.h, d.transparent) {
            Ok(()) => self.status = format!("Saved {}", p.display()),
            Err(e) => self.error = Some(format!("Export failed: {e:#}")),
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let Some(doc) = self.session.doc.as_ref() else {
            ui.label("No model loaded.");
            return false;
        };
        let results: Vec<String> = doc.results.iter().map(|r| r.name.clone()).collect();
        let vec_results: Vec<String> =
            std::iter::once(NONE_LABEL.to_string()).chain(doc.results.iter().filter(|r| r.is_deform_source()).map(|r| r.name.clone())).collect();
        let cur = doc.result(&self.session.opts.result).or_else(|| doc.results.first());
        let (comps, steps): (Vec<String>, Vec<String>) = match cur {
            Some(r) => {
                (r.choices(), r.step_labels())
            }
            None => (vec![], vec![]),
        };
        let o = &mut self.session.opts;
        egui::Grid::new("sel").num_columns(2).show(ui, |ui| {
            changed |= combo(ui, "Contour result", &mut o.result, &results);
            changed |= combo(ui, "Component", &mut o.component, &comps);
            changed |= combo(ui, "Step", &mut o.step, &steps);
            changed |= combo(ui, "Deform with", &mut o.deform_with, &vec_results);
        });
        ui.separator();
        changed |= ui.checkbox(&mut o.show_deformation, "Show deformation").changed();
        changed |= ui.checkbox(&mut o.auto_scale, "Auto scale").changed();
        ui.horizontal(|ui| {
            ui.label("Factor");
            changed |= ui
                .add_enabled(!o.auto_scale, egui::DragValue::new(&mut o.scale_factor).speed(0.05).max_decimals(4).range(0.0..=1e9))
                .changed();
        });
        ui.separator();
        changed |= ui.checkbox(&mut o.show_outline, "Show edges (outline)").changed();
        changed |= ui.checkbox(&mut o.show_edges, "Show mesh edges").changed();
        changed |= ui.checkbox(&mut o.show_undeformed, "Show undeformed outline").changed();
        ui.separator();
        changed |= ui.checkbox(&mut o.fixed_range, "Fixed contour range").changed();
        let speed = ((o.range_max - o.range_min).abs() * 0.005).max(1e-12);
        ui.add_enabled_ui(o.fixed_range, |ui| {
            egui::Grid::new("range").num_columns(2).show(ui, |ui| {
                ui.label("Min");
                changed |= ui.add(egui::DragValue::new(&mut o.range_min).speed(speed).max_decimals(8)).changed();
                ui.end_row();
                ui.label("Max");
                changed |= ui.add(egui::DragValue::new(&mut o.range_max).speed(speed).max_decimals(8)).changed();
                ui.end_row();
            });
        });
        if let Some(sc) = &self.session.scene {
            if sc.have_result {
                ui.separator();
                ui.label(format!("Min {}\nMax {}", crate::gidio::fmt_g(sc.data_range.0, 6), crate::gidio::fmt_g(sc.data_range.1, 6)));
            }
        }
        changed
    }

    fn viewport(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let ppp = ctx.pixels_per_point();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::WHITE);
        if self.session.doc.is_none() {
            painter.text(rect.center(), Align2::CENTER_CENTER, "File ▸ Open…  (.msh / .res / .frd)", FontId::proportional(18.0), Color32::GRAY);
            return;
        }
        if self.session.opts.show_toolbar {
            self.toolbar(&ctx, rect);
        }
        // --- camera interaction
        let d = resp.drag_delta();
        let mods = ctx.input(|i| i.modifiers);
        let h = rect.height() as f64;
        if self.zoom_box {
            // rubber-band zoom: drag a rectangle to zoom into it
            if resp.drag_started_by(PointerButton::Primary) {
                self.box_start = resp.interact_pointer_pos();
            }
            if let (Some(a), Some(b)) = (self.box_start, resp.interact_pointer_pos()) {
                let r = Rect::from_two_pos(a, b);
                painter.rect(r, 0.0, Color32::from_rgba_unmultiplied(40, 110, 220, 40), Stroke::new(1.0, Color32::from_rgb(40, 110, 220)), egui::StrokeKind::Inside);
                if resp.drag_stopped_by(PointerButton::Primary) {
                    if r.width() > 4.0 && r.height() > 4.0 {
                        let c = rect.center();
                        self.session.camera.pan((c.x - r.center().x) as f64, (c.y - r.center().y) as f64, h);
                        let f = (r.width() / rect.width()).max(r.height() / rect.height()) as f64;
                        self.session.camera.dist *= f.clamp(1e-4, 1.0);
                    }
                    self.box_start = None;
                    self.zoom_box = false;
                }
            }
        } else if resp.dragged_by(PointerButton::Primary) {
            if mods.shift {
                self.session.camera.pan(d.x as f64, d.y as f64, h);
            } else if mods.ctrl || mods.mac_cmd {
                self.session.camera.spin(d.x as f64);
            } else {
                self.session.camera.orbit(d.x as f64, d.y as f64);
            }
        } else if resp.dragged_by(PointerButton::Secondary) || resp.dragged_by(PointerButton::Middle) {
            self.session.camera.pan(d.x as f64, d.y as f64, h);
        }
        if resp.hovered() {
            let (scroll, pinch) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            if scroll != 0.0 {
                self.session.camera.zoom(scroll as f64);
            }
            if pinch != 1.0 {
                self.session.camera.dist /= pinch as f64;
            }
        }
        // --- render
        let size = (((rect.width() * ppp) as u32).max(1), ((rect.height() * ppp) as u32).max(1));
        self.view_px = size;
        if let Some(p) = self.session.draw_params(size, false) {
            match self.renderer.render(&p) {
                Ok(()) => {
                    if self.tex_id.is_none() {
                        // registered once; the renderer resizes the texture storage in place
                        self.tex_id = Some(frame.register_native_glow_texture(self.renderer.display_texture()));
                    }
                }
                Err(e) => {
                    if self.error.is_none() {
                        self.error = Some(format!("Rendering failed: {e:#}"));
                    }
                }
            }
        }
        if resp.double_clicked_by(PointerButton::Primary) && !self.zoom_box {
            self.session.fit();
        }
        if let Some(id) = self.tex_id {
            painter.image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        for p in self.session.overlay_prims(rect.width(), rect.height()) {
            let at = |x: f32, y: f32| rect.min + Vec2::new(x, y);
            let c = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
            match p {
                Prim::Rect { x, y, w, h, col } => {
                    painter.rect_filled(Rect::from_min_size(at(x, y), Vec2::new(w, h)), 0.0, c(col));
                }
                Prim::Line { a, b, col, width } => {
                    painter.line_segment([at(a[0], a[1]), at(b[0], b[1])], Stroke::new(width, c(col)));
                }
                Prim::Text { x, y, size, col, text } => {
                    painter.text(at(x, y), Align2::LEFT_TOP, text, FontId::proportional(size), c(col));
                }
            }
        }
    }

    fn toolbar(&mut self, ctx: &egui::Context, rect: Rect) {
        egui::Area::new(egui::Id::unique("viewport_toolbar"))
            .fixed_pos(rect.min + Vec2::new(10.0, 10.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button(" + ").on_hover_text("Zoom in").clicked() {
                            self.session.camera.dist *= 0.8;
                        }
                        if ui.button(" - ").on_hover_text("Zoom out").clicked() {
                            self.session.camera.dist *= 1.25;
                        }
                        if ui.selectable_label(self.zoom_box, "Zoom box").on_hover_text("Drag a rectangle to zoom into it").clicked() {
                            self.zoom_box = !self.zoom_box;
                            self.box_start = None;
                        }
                        if ui.button("Frame").on_hover_text("Centre the object in the view and fit it (double-click the view)").clicked() {
                            self.session.fit();
                        }
                        ui.separator();
                        for (label, v, tip) in [
                            ("Iso", StandardView::Isometric, "Isometric view"),
                            ("Front", StandardView::Front, "View from -Y"),
                            ("Right", StandardView::Right, "View from +X"),
                            ("Top", StandardView::Top, "View from +Z"),
                        ] {
                            if ui.button(label).on_hover_text(tip).clicked() {
                                self.session.set_standard_view(v);
                            }
                        }
                    });
                });
            });
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        egui::Window::new("About Postmesh")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("Postmesh");
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.add_space(6.0);
                    ui.label("Postprocessor for finite-element results:");
                    ui.label("deformed shapes with contour plots.");
                    ui.label("Reads GiD (.msh / .res) and CalculiX (.frd) files.");
                    ui.add_space(8.0);
                    ui.label("Developed by Carlos Couto");
                    ui.hyperlink_to("ccouto@ua.pt", "mailto:ccouto@ua.pt");
                    ui.add_space(8.0);
                    ui.small("Built with Rust, egui and OpenGL.");
                });
            });
        self.about_open = open;
    }

    fn options_window(&mut self, ctx: &egui::Context) {
        if !self.options_open {
            return;
        }
        let mut open = true;
        let before = self.prefs.clone();
        let cmap_before = self.session.opts.colormap.clone();
        egui::Window::new("Options").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Colormap");
                egui::ComboBox::from_id_salt("colormap").selected_text(self.session.opts.colormap.clone()).show_ui(ui, |ui| {
                    for n in colormap::NAMES {
                        ui.selectable_value(&mut self.session.opts.colormap, n.to_string(), n);
                    }
                });
            });
            ui.separator();
            ui.checkbox(&mut self.prefs.save_gv, "Create and update a .gv view file next to each model");
            ui.checkbox(&mut self.prefs.restore_gv, "Restore the view from the .gv file when it exists");
            ui.separator();
            ui.small("The .gv file stores the view options and camera of a model\n(same name as the .msh, extension .gv).");
            if let Some(p) = &self.gv_path {
                ui.small(format!("Current: {}", p.display()));
            }
        });
        if self.session.opts.colormap != cmap_before {
            self.session.recompute(&mut self.renderer);
        }
        if self.prefs != before {
            self.prefs.save();
        }
        self.options_open = open;
    }

    fn export_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.export.take() else { return };
        let mut keep = true;
        let mut go = false;
        egui::Window::new("Export image").collapsible(false).resizable(false).show(ctx, |ui| {
            egui::Grid::new("exp").num_columns(2).show(ui, |ui| {
                ui.label("Format");
                egui::ComboBox::from_id_salt("fmt").selected_text(EXPORT_FORMATS[d.format_idx].1).show_ui(ui, |ui| {
                    for (i, (_, label)) in EXPORT_FORMATS.iter().enumerate() {
                        ui.selectable_value(&mut d.format_idx, i, *label);
                    }
                });
                ui.end_row();
                let vector = VECTOR_FORMATS.contains(&EXPORT_FORMATS[d.format_idx].0);
                ui.label(if vector { "Page width (pt)" } else { "Width (px)" });
                if ui.add(egui::DragValue::new(&mut d.w).range(100..=16000)).changed() {
                    d.h = ((d.w as f32 * d.aspect).round() as u32).clamp(100, 16000);
                }
                ui.end_row();
                ui.label(if vector { "Page height (pt)" } else { "Height (px)" });
                if ui.add(egui::DragValue::new(&mut d.h).range(100..=16000)).changed() {
                    d.w = ((d.h as f32 / d.aspect).round() as u32).clamp(100, 16000);
                }
                ui.end_row();
            });
            let ext = EXPORT_FORMATS[d.format_idx].0;
            ui.add_enabled(matches!(ext, "png" | "tiff" | "svg" | "pdf" | "eps" | "ps"), egui::Checkbox::new(&mut d.transparent, "Transparent background"));
            if VECTOR_FORMATS.contains(&ext) {
                ui.small("Vector output keeps the surface as scalable polygons;\nlarge meshes give large files.");
            }
            ui.horizontal(|ui| {
                if ui.button("Export…").clicked() {
                    go = true;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if go {
            self.do_export(&d);
        } else if keep {
            self.export = Some(d);
        }
    }
}

fn combo(ui: &mut egui::Ui, label: &str, cur: &mut String, items: &[String]) -> bool {
    let mut changed = false;
    ui.label(label);
    egui::ComboBox::from_id_salt(label).selected_text(cur.clone()).width(150.0).show_ui(ui, |ui| {
        for it in items {
            if ui.selectable_label(cur == it, it).clicked() && cur != it {
                *cur = it.clone();
                changed = true;
            }
        }
    });
    ui.end_row();
    changed
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(path) = self.gui_screenshot.clone() {
            self.frames += 1;
            if self.frames == 12 {
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                        let _ = buf.save(&path);
                    }
                    std::process::exit(0);
                });
            }
            if self.frames == 3 && std::env::var_os("POSTMESH_TEST_ABOUT").is_some() {
                self.about_open = true;
            }
            // test hook: POSTMESH_TEST_TOUCH=1 simulates an external edit followed by a focus event
            if std::env::var_os("POSTMESH_TEST_TOUCH").is_some() {
                if self.frames == 5 {
                    for f in self.source_files() {
                        if let Ok(h) = std::fs::File::options().write(true).open(&f) {
                            let _ = h.set_modified(std::time::SystemTime::now());
                        }
                    }
                }
                if self.frames == 8 {
                    self.check_files();
                }
            }
            ctx.request_repaint();
        }
        // window regained focus: has the model changed on disk?
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.was_focused {
            self.check_files();
        }
        self.was_focused = focused;
        // drag & drop: open the first dropped .msh/.res file
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            match dropped.iter().find(|p| {
                p.extension().is_some_and(|e| {
                    ["msh", "res", "frd"].iter().any(|x| e.eq_ignore_ascii_case(x))
                })
            }) {
                Some(p) => self.open(p),
                None => self.error = Some("Drop a GiD .msh/.res or CalculiX .frd file.".into()),
            }
        }
        let (open, exp) = ctx.input_mut(|i| (i.consume_key(Modifiers::COMMAND, Key::O), i.consume_key(Modifiers::COMMAND, Key::E)));
        if open {
            self.open_dialog();
        }
        let can_export = self.session.doc.is_some();
        let start_export = |app: &mut App| {
            let (w, h) = app.view_px;
            let pf = &app.prefs;
            let (w, h) = if pf.export_width >= 100 && pf.export_height >= 100 { (pf.export_width, pf.export_height) } else { (w * 2, h * 2) };
            let format_idx = EXPORT_FORMATS.iter().position(|(e, _)| *e == pf.export_format).unwrap_or(0);
            app.export = Some(ExportDialog { format_idx, w, h, aspect: h as f32 / w.max(1) as f32, transparent: pf.export_transparent });
        };
        if exp && can_export {
            start_export(self);
        }

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…").clicked() {
                        ui.close();
                        self.open_dialog();
                    }
                    if ui.add_enabled(can_export, egui::Button::new("Export image…")).clicked() {
                        ui.close();
                        start_export(self);
                    }
                    ui.separator();
                    if ui.button("Export view options…").clicked() {
                        ui.close();
                        self.export_view();
                    }
                    if ui.button("Import view options…").clicked() {
                        ui.close();
                        self.import_view();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Options…").clicked() {
                        ui.close();
                        self.options_open = true;
                    }
                });
                ui.menu_button("View", |ui| {
                    for (label, v) in [
                        ("Isometric", StandardView::Isometric),
                        ("Front (-Y)", StandardView::Front),
                        ("Right (+X)", StandardView::Right),
                        ("Top (+Z)", StandardView::Top),
                    ] {
                        if ui.button(label).clicked() {
                            ui.close();
                            self.session.set_standard_view(v);
                        }
                    }
                    if ui.button("Fit").clicked() {
                        ui.close();
                        self.session.fit();
                    }
                    ui.separator();
                    let o = &mut self.session.opts;
                    ui.checkbox(&mut o.show_legend, "Legend");
                    ui.checkbox(&mut o.show_caption, "Caption");
                    ui.checkbox(&mut o.show_axes, "Axes");
                    ui.checkbox(&mut o.show_toolbar, "Toolbar");
                    let any = o.show_legend || o.show_caption || o.show_axes || o.show_toolbar;
                    if ui.button(if any { "Hide all visuals" } else { "Show all visuals" }).clicked() {
                        o.show_legend = !any;
                        o.show_caption = !any;
                        o.show_axes = !any;
                        o.show_toolbar = !any;
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About Postmesh").clicked() {
                        ui.close();
                        self.about_open = true;
                    }
                });
            });
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(&self.status);
        });
        egui::Panel::left("controls").default_size(270.0).resizable(false).show(ui, |ui| {
            if self.controls(ui) {
                self.session.recompute(&mut self.renderer);
            }
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.viewport(ui, frame));
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let r = ctx.content_rect();
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop_hint")));
            p.rect_filled(r, 0.0, Color32::from_black_alpha(140));
            p.text(r.center(), Align2::CENTER_CENTER, "Drop .msh / .res / .frd file to open", FontId::proportional(28.0), Color32::WHITE);
        }
        self.export_window(&ctx);
        self.options_window(&ctx);
        self.reload_window(&ctx);
        self.about_window(&ctx);
        // keep the .gv in step with the view once the mouse is released
        if !ctx.input(|i| i.pointer.any_down()) {
            self.sync_gv(false);
        }
        if self.session.opts != self.last_saved {
            view::save_settings(&self.session.opts);
            self.last_saved = self.session.opts.clone();
        }
        if let Some(e) = self.error.clone() {
            egui::Window::new("Error").collapsible(false).show(&ctx, |ui| {
                ui.label(&e);
                if ui.button("OK").clicked() {
                    self.error = None;
                }
            });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.sync_gv(true);
        view::save_settings(&self.session.snapshot());
    }
}


fn stamps(files: &[PathBuf]) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
    files.iter().map(|p| (p.clone(), std::fs::metadata(p).and_then(|m| m.modified()).ok())).collect()
}

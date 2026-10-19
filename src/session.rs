//! The current model, view options and camera, shared by the GUI and the headless exporter.

use crate::camera::{Camera, StandardView};
use crate::doc::{self, Document, NONE_LABEL, SceneData};
use crate::overlay::{self, OverlayInfo, Prim};
use crate::render::{DrawParams, Renderer};
use crate::vector::{self, VectorScene};
use crate::view::ViewOptions;
use anyhow::{Result, bail};
use glam::DVec3;
use image::RgbaImage;
use std::path::Path;

pub struct Session {
    pub doc: Option<Document>,
    pub opts: ViewOptions,
    pub scene: Option<SceneData>,
    pub camera: Camera,
    /// camera to apply when the next model is loaded (restored settings / imported view)
    pub camera_pending: Option<[[f64; 3]; 3]>,
}

pub const VECTOR_FORMATS: [&str; 4] = ["svg", "pdf", "eps", "ps"];

impl Session {
    pub fn new(opts: ViewOptions) -> Self {
        let camera_pending = opts.camera;
        Session { doc: None, opts, scene: None, camera: Camera::new(DVec3::ZERO, 1.0), camera_pending }
    }

    /// Load a model and (re)apply the current view choices to it.
    pub fn load(&mut self, path: &Path, renderer: &mut Renderer) -> Result<()> {
        let d = Document::load(path)?;
        renderer.set_surface(&d.surface, d.mesh.coords.len());
        // sensible default for the deformation source
        let has = |n: &str| d.result(n).is_some_and(|r| r.is_deform_source());
        if self.opts.deform_with != NONE_LABEL && !has(&self.opts.deform_with) {
            self.opts.deform_with = d.default_deform().unwrap_or_else(|| NONE_LABEL.into());
        }
        self.doc = Some(d);
        self.recompute(renderer);
        let (c, r) = self.bounds().unwrap();
        self.camera = Camera::new(c, r);
        if let Some(cam) = self.camera_pending.take() {
            self.camera.from_json(cam);
        }
        Ok(())
    }

    pub fn recompute(&mut self, renderer: &mut Renderer) {
        let Some(d) = &self.doc else { return };
        let sc = doc::compute_scene(d, &mut self.opts);
        renderer.set_vertices(&sc.positions, &sc.scalars, &sc.rest_positions);
        renderer.set_colormap(&self.opts.colormap);
        self.scene = Some(sc);
        if let Some((_, r)) = self.bounds() {
            self.camera.scene_radius = r;
        }
    }

    /// Bounding sphere (centre in world coordinates, radius) of what is currently shown.
    pub fn bounds(&self) -> Option<(DVec3, f64)> {
        let d = self.doc.as_ref()?;
        let sc = self.scene.as_ref()?;
        let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
        let mut add = |ps: &[[f32; 3]]| {
            for p in ps {
                let v = glam::Vec3::from_array(*p);
                lo = lo.min(v);
                hi = hi.max(v);
            }
        };
        add(&sc.positions);
        if self.opts.show_undeformed && sc.scale > 0.0 {
            add(&sc.rest_positions);
        }
        let c = d.center + ((lo + hi) * 0.5).as_dvec3();
        Some((c, (((hi - lo).length() as f64) * 0.5).max(1e-9)))
    }

    /// Use these options (and camera) for the next model that is loaded.
    pub fn set_view_for_load(&mut self, v: ViewOptions) {
        self.camera_pending = v.camera;
        self.opts = ViewOptions { camera: None, ..v };
    }

    pub fn apply_view(&mut self, v: ViewOptions, renderer: &mut Renderer) {
        let cam = v.camera;
        self.opts = ViewOptions { camera: None, ..v };
        if let (Some(c), Some(_)) = (cam, &self.doc) {
            self.camera.from_json(c);
        } else if cam.is_some() {
            self.camera_pending = cam;
        }
        self.recompute(renderer);
    }

    /// Options including the current camera, for saving.
    pub fn snapshot(&self) -> ViewOptions {
        let mut v = self.opts.clone();
        v.camera = if self.doc.is_some() { Some(self.camera.to_json()) } else { self.camera_pending };
        v
    }

    pub fn set_standard_view(&mut self, v: StandardView) {
        if let Some((c, r)) = self.bounds() {
            self.camera.set_standard(v, c, r);
        }
    }

    pub fn fit(&mut self) {
        if let Some((c, r)) = self.bounds() {
            self.camera.fit(c, r);
        }
    }

    pub fn draw_params(&self, size: (u32, u32), transparent: bool) -> Option<DrawParams> {
        let d = self.doc.as_ref()?;
        let sc = self.scene.as_ref()?;
        let aspect = size.0 as f64 / size.1.max(1) as f64;
        let eye = (self.camera.eye() - d.center).as_vec3();
        Some(DrawParams {
            mvp: self.camera.mvp(aspect, d.center),
            eye: eye.to_array(),
            clim: (sc.clim.0 as f32, sc.clim.1 as f32),
            shaded: true,
            show_edges: self.opts.show_edges,
            show_outline: self.opts.show_outline,
            show_ghost: self.opts.show_undeformed && sc.scale > 0.0,
            size,
            transparent,
        })
    }

    pub fn overlay_info(&self) -> Option<OverlayInfo> {
        let sc = self.scene.as_ref()?;
        Some(OverlayInfo {
            title: sc.title.clone(),
            caption: sc.caption.clone(),
            clim: sc.clim,
            colormap: self.opts.colormap.clone(),
            have_result: sc.have_result,
            show_legend: self.opts.show_legend,
            show_caption: self.opts.show_caption,
            show_axes: self.opts.show_axes,
            axes: [
                self.camera.axis_screen(DVec3::X),
                self.camera.axis_screen(DVec3::Y),
                self.camera.axis_screen(DVec3::Z),
            ],
        })
    }

    pub fn overlay_prims(&self, w: f32, h: f32) -> Vec<Prim> {
        self.overlay_info().map(|i| overlay::layout(w, h, &i)).unwrap_or_default()
    }

    /// Render the scene (with legend etc.) into an RGBA image of the given size.
    pub fn render_image(&self, renderer: &mut Renderer, w: u32, h: u32, transparent: bool) -> Result<RgbaImage> {
        let Some(p) = self.draw_params((w, h), transparent) else { bail!("no model loaded") };
        renderer.render(&p)?;
        let (w, h, mut px) = renderer.read_pixels()?;
        if transparent {
            // un-premultiply the MSAA-resolved edge pixels
            for c in px.chunks_exact_mut(4) {
                let a = c[3] as u32;
                if a > 0 && a < 255 {
                    for k in 0..3 {
                        c[k] = ((c[k] as u32 * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
        }
        let mut img = RgbaImage::from_raw(w, h, px).ok_or_else(|| anyhow::anyhow!("bad image buffer"))?;
        overlay::draw_raster(&mut img, &self.overlay_prims(w as f32, h as f32));
        Ok(img)
    }

    pub fn export(&self, renderer: &mut Renderer, path: &Path, fmt: &str, w: u32, h: u32, transparent: bool) -> Result<()> {
        let (Some(d), Some(sc)) = (&self.doc, &self.scene) else { bail!("no model loaded") };
        if VECTOR_FORMATS.contains(&fmt) {
            let vs = VectorScene {
                w: w as f32,
                h: h as f32,
                mvp: self.camera.mvp(w as f64 / h.max(1) as f64, d.center),
                positions: &sc.positions,
                rest: &sc.rest_positions,
                scalars: &sc.scalars,
                surface: &d.surface,
                clim: (sc.clim.0 as f32, sc.clim.1 as f32),
                colormap: &self.opts.colormap,
                have_result: sc.have_result,
                show_edges: self.opts.show_edges,
                show_outline: self.opts.show_outline,
                show_ghost: self.opts.show_undeformed && sc.scale > 0.0,
                transparent,
                overlay: self.overlay_prims(w as f32, h as f32),
            };
            return vector::export(path, fmt, &vs);
        }
        let transparent = transparent && matches!(fmt, "png" | "tiff");
        let img = self.render_image(renderer, w, h, transparent)?;
        match fmt {
            "jpg" | "jpeg" => image::DynamicImage::ImageRgba8(img).to_rgb8().save_with_format(path, image::ImageFormat::Jpeg)?,
            "png" => img.save_with_format(path, image::ImageFormat::Png)?,
            "tiff" => img.save_with_format(path, image::ImageFormat::Tiff)?,
            "bmp" => image::DynamicImage::ImageRgba8(img).to_rgb8().save_with_format(path, image::ImageFormat::Bmp)?,
            other => bail!("unknown format '{other}'"),
        }
        Ok(())
    }
}

pub fn format_of(path: &Path) -> String {
    match path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("tif" | "tiff") => "tiff".into(),
        Some("jpg" | "jpeg") => "jpg".into(),
        Some(e) => e.into(),
        None => String::new(),
    }
}

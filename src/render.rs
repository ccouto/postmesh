//! OpenGL (glow) renderer: contour-filled surface, mesh edges, outline and undeformed outline,
//! drawn off-screen into a texture (used for both the live view and image export).

use crate::colormap;
use crate::doc::Surface;
use anyhow::{Result, anyhow, bail};
use bytemuck::{Pod, Zeroable};
use eframe::glow::{self, HasContext};
use glam::Mat4;
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    s: f32,
}

pub struct DrawParams {
    pub mvp: Mat4,
    pub eye: [f32; 3],
    pub clim: (f32, f32),
    pub shaded: bool,
    pub show_edges: bool,
    pub show_outline: bool,
    pub show_ghost: bool,
    pub size: (u32, u32),
    pub transparent: bool,
}

const VERT: &str = r#"
layout(location = 0) in vec3 p;
layout(location = 1) in float s;
uniform mat4 mvp;
out float v_s;
out vec3 v_wp;
void main() {
    vec4 c = mvp * vec4(p, 1.0);
    // projection is built for a 0..1 depth range; map to OpenGL's -1..1
    c.z = c.z * 2.0 - c.w;
    // render upside down so that texture row 0 / readback row 0 is the top of the image
    c.y = -c.y;
    gl_Position = c;
    v_s = s;
    v_wp = p;
}
"#;

const FRAG_TRI: &str = r#"
in float v_s;
in vec3 v_wp;
uniform vec2 clim;
uniform float shade;
uniform vec3 eye;
uniform float lut_size;
uniform sampler2D lut;
out vec4 frag;
void main() {
    float t = clamp((v_s - clim.x) / max(clim.y - clim.x, 1e-30), 0.0, 1.0);
    vec3 c = textureLod(lut, vec2((t * (lut_size - 1.0) + 0.5) / lut_size, 0.5), 0.0).rgb;
    if (shade > 0.5) {
        vec3 n = normalize(cross(dFdx(v_wp), dFdy(v_wp)));
        vec3 v = normalize(eye - v_wp);
        c *= 0.72 + 0.28 * abs(dot(n, v));
    }
    frag = vec4(c, 1.0);
}
"#;

const FRAG_LINE: &str = r#"
in float v_s;
in vec3 v_wp;
uniform vec4 color;
out vec4 frag;
void main() {
    frag = color;
}
"#;

struct Targets {
    size: (u32, u32),
    samples: i32,
    resolve_fbo: glow::Framebuffer,
    msaa_fbo: Option<glow::Framebuffer>,
    rb_color: Option<glow::Renderbuffer>,
    rb_depth: glow::Renderbuffer,
}

pub struct Renderer {
    gl: Arc<glow::Context>,
    tri: glow::Program,
    line: glow::Program,
    lut_tex: glow::Texture,
    /// colour texture shown by egui (owned by egui once registered; resized in place)
    out_tex: glow::Texture,
    vbuf: Option<glow::Buffer>,
    ghost_buf: Option<glow::Buffer>,
    ibufs: Vec<glow::Buffer>,
    vao_tri: Option<glow::VertexArray>,
    vao_edge: Option<glow::VertexArray>,
    vao_feat: Option<glow::VertexArray>,
    vao_ghost: Option<glow::VertexArray>,
    vaos: Vec<glow::VertexArray>,
    n_tri_idx: i32,
    n_edge_idx: i32,
    n_feat_idx: i32,
    targets: Option<Targets>,
    max_samples: i32,
    max_size: i32,
}

fn gl_check(g: &glow::Context, what: &str) {
    if std::env::var_os("POSTMESH_GL_DEBUG").is_some() {
        let e = unsafe { g.get_error() };
        if e != 0 {
            eprintln!("GL error 0x{e:x} after {what}");
        }
    }
}

fn shader_header(gl: &glow::Context) -> &'static str {
    if gl.version().is_embedded {
        "#version 300 es\nprecision highp float;\nprecision highp sampler2D;\n"
    } else {
        "#version 330 core\n"
    }
}

fn compile(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program> {
    unsafe {
        let head = shader_header(gl);
        let prog = gl.create_program().map_err(|e| anyhow!(e))?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let sh = gl.create_shader(kind).map_err(|e| anyhow!(e))?;
            gl.shader_source(sh, &format!("{head}{src}"));
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                bail!("shader compile error: {}", gl.get_shader_info_log(sh));
            }
            gl.attach_shader(prog, sh);
            shaders.push(sh);
        }
        gl.link_program(prog);
        if !gl.get_program_link_status(prog) {
            bail!("shader link error: {}", gl.get_program_info_log(prog));
        }
        for sh in shaders {
            gl.detach_shader(prog, sh);
            gl.delete_shader(sh);
        }
        Ok(prog)
    }
}

fn setup_tex(g: &glow::Context, tex: glow::Texture) {
    unsafe {
        g.bind_texture(glow::TEXTURE_2D, Some(tex));
        g.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        g.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        g.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        g.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    }
}

impl Renderer {
    pub fn new(gl: &Arc<glow::Context>) -> Result<Self> {
        let g = gl.as_ref();
        unsafe {
            let tri = compile(g, VERT, FRAG_TRI)?;
            let line = compile(g, VERT, FRAG_LINE)?;
            let lut_tex = g.create_texture().map_err(|e| anyhow!(e))?;
            setup_tex(g, lut_tex);
            let out_tex = g.create_texture().map_err(|e| anyhow!(e))?;
            setup_tex(g, out_tex);
            // minimal storage so the texture is valid before the first render
            g.tex_image_2d(glow::TEXTURE_2D, 0, glow::SRGB8_ALPHA8 as i32, 1, 1, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            g.bind_texture(glow::TEXTURE_2D, None);
            let max_samples = g.get_parameter_i32(glow::MAX_SAMPLES).max(0);
            let max_size = g.get_parameter_i32(glow::MAX_RENDERBUFFER_SIZE).max(1024);
            let mut r = Self {
                gl: gl.clone(),
                tri,
                line,
                lut_tex,
                out_tex,
                vbuf: None,
                ghost_buf: None,
                ibufs: Vec::new(),
                vao_tri: None,
                vao_edge: None,
                vao_feat: None,
                vao_ghost: None,
                vaos: Vec::new(),
                n_tri_idx: 0,
                n_edge_idx: 0,
                n_feat_idx: 0,
                targets: None,
                max_samples,
                max_size,
            };
            r.set_colormap(colormap::NAMES[0]);
            Ok(r)
        }
    }

    /// Texture to register with egui (once). Its storage is re-specified in place on resize.
    pub fn display_texture(&self) -> glow::Texture {
        self.out_tex
    }

    pub fn set_colormap(&mut self, name: &str) {
        let lut = colormap::lut(name);
        unsafe {
            let g = self.gl.as_ref();
            g.bind_texture(glow::TEXTURE_2D, Some(self.lut_tex));
            g.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            g.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                colormap::LUT_SIZE as i32,
                1,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(bytemuck::cast_slice(&lut))),
            );
            g.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    fn free_geometry(&mut self) {
        unsafe {
            let g = self.gl.as_ref();
            for b in self.ibufs.drain(..) {
                g.delete_buffer(b);
            }
            for v in self.vaos.drain(..) {
                g.delete_vertex_array(v);
            }
            if let Some(b) = self.vbuf.take() {
                g.delete_buffer(b);
            }
            if let Some(b) = self.ghost_buf.take() {
                g.delete_buffer(b);
            }
        }
        self.vao_tri = None;
        self.vao_edge = None;
        self.vao_feat = None;
        self.vao_ghost = None;
    }

    /// Upload the (static) surface topology of a newly loaded model.
    pub fn set_surface(&mut self, surf: &Surface, nverts: usize) {
        self.free_geometry();
        let mut tris: Vec<u32> = Vec::with_capacity(surf.polys.len() * 6);
        for p in &surf.polys {
            tris.extend_from_slice(&[p[0], p[1], p[2]]);
            if p[3] != u32::MAX {
                tris.extend_from_slice(&[p[0], p[2], p[3]]);
            }
        }
        let flat = |e: &Vec<[u32; 2]>| e.iter().flatten().copied().collect::<Vec<u32>>();
        let (edges, feat) = (flat(&surf.edges), flat(&surf.feature_edges));
        unsafe {
            let g = self.gl.as_ref();
            let vsize = (nverts * std::mem::size_of::<Vertex>()) as i32;
            let mk_vbuf = |g: &glow::Context| {
                let b = g.create_buffer().unwrap();
                g.bind_buffer(glow::ARRAY_BUFFER, Some(b));
                g.buffer_data_size(glow::ARRAY_BUFFER, vsize, glow::DYNAMIC_DRAW);
                b
            };
            let vb = mk_vbuf(g);
            let gb = mk_vbuf(g);
            let mk_vao = |g: &glow::Context,
                          vbuf: glow::Buffer,
                          idx: &[u32],
                          ibufs: &mut Vec<glow::Buffer>,
                          vaos: &mut Vec<glow::VertexArray>|
             -> Option<glow::VertexArray> {
                if idx.is_empty() {
                    return None;
                }
                let vao = g.create_vertex_array().ok()?;
                g.bind_vertex_array(Some(vao));
                g.bind_buffer(glow::ARRAY_BUFFER, Some(vbuf));
                g.enable_vertex_attrib_array(0);
                g.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 16, 0);
                g.enable_vertex_attrib_array(1);
                g.vertex_attrib_pointer_f32(1, 1, glow::FLOAT, false, 16, 12);
                let ib = g.create_buffer().ok()?;
                g.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ib));
                g.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, bytemuck::cast_slice(idx), glow::STATIC_DRAW);
                g.bind_vertex_array(None);
                ibufs.push(ib);
                vaos.push(vao);
                Some(vao)
            };
            let (mut ibufs, mut vaos) = (Vec::new(), Vec::new());
            self.vao_tri = mk_vao(g, vb, &tris, &mut ibufs, &mut vaos);
            self.vao_edge = mk_vao(g, vb, &edges, &mut ibufs, &mut vaos);
            self.vao_feat = mk_vao(g, vb, &feat, &mut ibufs, &mut vaos);
            self.vao_ghost = mk_vao(g, gb, &feat, &mut ibufs, &mut vaos);
            g.bind_buffer(glow::ARRAY_BUFFER, None);
            g.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
            self.ibufs = ibufs;
            self.vaos = vaos;
            self.vbuf = Some(vb);
            self.ghost_buf = Some(gb);
        }
        self.n_tri_idx = tris.len() as i32;
        self.n_edge_idx = edges.len() as i32;
        self.n_feat_idx = feat.len() as i32;
    }

    pub fn set_vertices(&mut self, positions: &[[f32; 3]], scalars: &[f32], rest: &[[f32; 3]]) {
        let (Some(vb), Some(gb)) = (self.vbuf, self.ghost_buf) else { return };
        let v: Vec<Vertex> = positions.iter().zip(scalars).map(|(p, s)| Vertex { pos: *p, s: *s }).collect();
        let gv: Vec<Vertex> = rest.iter().map(|p| Vertex { pos: *p, s: 0.0 }).collect();
        unsafe {
            let g = self.gl.as_ref();
            g.bind_buffer(glow::ARRAY_BUFFER, Some(vb));
            g.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytemuck::cast_slice(&v));
            g.bind_buffer(glow::ARRAY_BUFFER, Some(gb));
            g.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytemuck::cast_slice(&gv));
            g.bind_buffer(glow::ARRAY_BUFFER, None);
        }
    }

    fn free_targets(&mut self) {
        if let Some(t) = self.targets.take() {
            unsafe {
                let g = self.gl.as_ref();
                g.delete_framebuffer(t.resolve_fbo);
                if let Some(f) = t.msaa_fbo {
                    g.delete_framebuffer(f);
                }
                if let Some(r) = t.rb_color {
                    g.delete_renderbuffer(r);
                }
                g.delete_renderbuffer(t.rb_depth);
            }
        }
    }

    fn ensure_targets(&mut self, size: (u32, u32)) -> Result<()> {
        if size.0 as i32 > self.max_size || size.1 as i32 > self.max_size {
            bail!("image size {}x{} exceeds this GPU's limit of {} pixels", size.0, size.1, self.max_size);
        }
        let want_samples = if (size.0 as u64) * (size.1 as u64) > 16_000_000 { 0 } else { self.max_samples.min(4) };
        let samples = if want_samples >= 2 { want_samples } else { 0 };
        if self.targets.as_ref().is_some_and(|t| t.size == size && t.samples == samples) {
            return Ok(());
        }
        self.free_targets();
        let (w, h) = (size.0 as i32, size.1 as i32);
        unsafe {
            let g = self.gl.as_ref();
            let prev_fbo = g.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING);
            // resolved colour texture (re-specified in place so egui keeps its registration)
            g.bind_texture(glow::TEXTURE_2D, Some(self.out_tex));
            g.tex_image_2d(glow::TEXTURE_2D, 0, glow::SRGB8_ALPHA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            g.bind_texture(glow::TEXTURE_2D, None);
            let resolve_fbo = g.create_framebuffer().map_err(|e| anyhow!(e))?;
            g.bind_framebuffer(glow::FRAMEBUFFER, Some(resolve_fbo));
            g.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(self.out_tex), 0);

            let rb_depth = g.create_renderbuffer().map_err(|e| anyhow!(e))?;
            g.bind_renderbuffer(glow::RENDERBUFFER, Some(rb_depth));
            let (mut msaa_fbo, mut rb_color) = (None, None);
            if samples > 0 {
                g.renderbuffer_storage_multisample(glow::RENDERBUFFER, samples, glow::DEPTH_COMPONENT24, w, h);
                let rc = g.create_renderbuffer().map_err(|e| anyhow!(e))?;
                g.bind_renderbuffer(glow::RENDERBUFFER, Some(rc));
                g.renderbuffer_storage_multisample(glow::RENDERBUFFER, samples, glow::SRGB8_ALPHA8, w, h);
                let f = g.create_framebuffer().map_err(|e| anyhow!(e))?;
                g.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                g.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::RENDERBUFFER, Some(rc));
                g.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, Some(rb_depth));
                if g.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                    bail!("multisampled framebuffer is incomplete");
                }
                msaa_fbo = Some(f);
                rb_color = Some(rc);
            } else {
                g.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, w, h);
                g.bind_framebuffer(glow::FRAMEBUFFER, Some(resolve_fbo));
                g.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, Some(rb_depth));
            }
            g.bind_framebuffer(glow::FRAMEBUFFER, Some(resolve_fbo));
            if g.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                bail!("framebuffer is incomplete");
            }
            g.bind_renderbuffer(glow::RENDERBUFFER, None);
            g.bind_framebuffer(glow::FRAMEBUFFER, prev_fbo);
            self.targets = Some(Targets { size, samples, resolve_fbo, msaa_fbo, rb_color, rb_depth });
        }
        Ok(())
    }

    pub fn render(&mut self, p: &DrawParams) -> Result<()> {
        let size = (p.size.0.max(1), p.size.1.max(1));
        self.ensure_targets(size)?;
        let t = self.targets.as_ref().unwrap();
        let (w, h) = (size.0 as i32, size.1 as i32);
        let g = self.gl.as_ref();
        unsafe {
            // remember the state egui expects back
            let prev_fbo = g.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING);
            let mut vp = [0i32; 4];
            g.get_parameter_i32_slice(glow::VIEWPORT, &mut vp);
            let srgb = !g.version().is_embedded && g.is_enabled(glow::FRAMEBUFFER_SRGB);

            if !g.version().is_embedded {
                // we store colours as-is in an sRGB-format target; egui decodes them when sampling
                g.disable(glow::FRAMEBUFFER_SRGB);
            }
            g.bind_framebuffer(glow::FRAMEBUFFER, Some(t.msaa_fbo.unwrap_or(t.resolve_fbo)));
            g.viewport(0, 0, w, h);
            g.disable(glow::SCISSOR_TEST);
            g.disable(glow::BLEND);
            g.disable(glow::CULL_FACE);
            g.color_mask(true, true, true, true);
            g.depth_mask(true);
            g.enable(glow::DEPTH_TEST);
            g.depth_func(glow::LEQUAL);
            if p.transparent {
                g.clear_color(0.0, 0.0, 0.0, 0.0);
            } else {
                g.clear_color(1.0, 1.0, 1.0, 1.0);
            }
            g.clear_depth_f32(1.0);
            g.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl_check(g, "clear");

            let mvp = p.mvp.to_cols_array();
            let set_mvp = |prog: glow::Program| {
                g.use_program(Some(prog));
                g.uniform_matrix_4_f32_slice(g.get_uniform_location(prog, "mvp").as_ref(), false, &mvp);
            };
            if let Some(vao) = self.vao_tri {
                set_mvp(self.tri);
                let u = |n: &str| g.get_uniform_location(self.tri, n);
                g.uniform_2_f32(u("clim").as_ref(), p.clim.0, p.clim.1);
                g.uniform_1_f32(u("shade").as_ref(), if p.shaded { 1.0 } else { 0.0 });
                g.uniform_3_f32(u("eye").as_ref(), p.eye[0], p.eye[1], p.eye[2]);
                g.uniform_1_f32(u("lut_size").as_ref(), colormap::LUT_SIZE as f32);
                g.active_texture(glow::TEXTURE0);
                g.bind_texture(glow::TEXTURE_2D, Some(self.lut_tex));
                g.uniform_1_i32(u("lut").as_ref(), 0);
                g.enable(glow::POLYGON_OFFSET_FILL);
                g.polygon_offset(1.5, 2.0);
                g.bind_vertex_array(Some(vao));
                g.draw_elements(glow::TRIANGLES, self.n_tri_idx, glow::UNSIGNED_INT, 0);
                g.disable(glow::POLYGON_OFFSET_FILL);
                g.bind_texture(glow::TEXTURE_2D, None);
            }
            set_mvp(self.line);
            let ucol = g.get_uniform_location(self.line, "color");
            if p.show_ghost {
                if let Some(vao) = self.vao_ghost {
                    g.uniform_4_f32(ucol.as_ref(), 0.55, 0.55, 0.55, 1.0);
                    g.bind_vertex_array(Some(vao));
                    g.draw_elements(glow::LINES, self.n_feat_idx, glow::UNSIGNED_INT, 0);
                }
            }
            let lines = if p.show_edges {
                self.vao_edge.map(|v| (v, self.n_edge_idx))
            } else if p.show_outline {
                self.vao_feat.map(|v| (v, self.n_feat_idx))
            } else {
                None
            };
            if let Some((vao, n)) = lines {
                g.uniform_4_f32(ucol.as_ref(), 0.0, 0.0, 0.0, 1.0);
                g.bind_vertex_array(Some(vao));
                g.draw_elements(glow::LINES, n, glow::UNSIGNED_INT, 0);
            }
            gl_check(g, "draw");
            g.bind_vertex_array(None);
            g.use_program(None);

            if let Some(msaa) = t.msaa_fbo {
                g.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(msaa));
                g.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(t.resolve_fbo));
                g.blit_framebuffer(0, 0, w, h, 0, 0, w, h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
            }

            gl_check(g, "blit");
            // restore
            g.disable(glow::DEPTH_TEST);
            if srgb {
                g.enable(glow::FRAMEBUFFER_SRGB);
            }
            g.bind_framebuffer(glow::FRAMEBUFFER, prev_fbo);
            g.viewport(vp[0], vp[1], vp[2], vp[3]);
        }
        Ok(())
    }

    /// Read the last rendered frame back as tightly packed RGBA8 (top row first).
    pub fn read_pixels(&self) -> Result<(u32, u32, Vec<u8>)> {
        let t = self.targets.as_ref().ok_or_else(|| anyhow!("nothing rendered"))?;
        let (w, h) = t.size;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let g = self.gl.as_ref();
        unsafe {
            let prev = g.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING);
            g.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(t.resolve_fbo));
            g.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            g.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut buf)));
            g.bind_framebuffer(glow::FRAMEBUFFER, prev);
        }
        Ok((w, h, buf))
    }
}

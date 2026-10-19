mod app;
mod camera;
mod colormap;
mod doc;
mod frd;
mod gidio;
mod overlay;
mod render;
mod session;
mod vector;
mod view;

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

const USAGE: &str = "Postmesh – postprocessor for finite-element results

usage: postmesh [model.msh|model.res] [--view view.json]
       postmesh model.msh --export out.(png|tiff|jpg|bmp|svg|pdf|eps|ps)
                    [--size WxH] [--view view.json] [--transparent]   (headless batch export)";

struct Args {
    file: Option<PathBuf>,
    view: Option<PathBuf>,
    export: Option<PathBuf>,
    size: Option<(u32, u32)>,
    transparent: bool,
    gui_screenshot: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let mut a = Args { file: None, view: None, export: None, size: None, transparent: false, gui_screenshot: None };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--view" => a.view = Some(it.next().context("--view needs a path")?.into()),
            "--export" => a.export = Some(it.next().context("--export needs a path")?.into()),
            "--transparent" => a.transparent = true,
            "--gui-screenshot" => a.gui_screenshot = Some(it.next().context("--gui-screenshot needs a path")?.into()),
            "--size" => {
                let s = it.next().context("--size needs WxH")?;
                let (w, h) = s.split_once(['x', 'X']).context("size must look like 1600x1000")?;
                a.size = Some((w.parse()?, h.parse()?));
            }
            f if !f.starts_with('-') => a.file = Some(f.into()),
            other => bail!("unknown argument {other}\n\n{USAGE}"),
        }
    }
    Ok(a)
}

/// Batch export: runs inside an (invisible) eframe window only to get an OpenGL context.
fn batch(gl: &std::sync::Arc<eframe::glow::Context>, args: Args) -> Result<()> {
    let file = args.file.context("--export needs a model file")?;
    let out = args.export.unwrap();
    let mut renderer = render::Renderer::new(gl)?;
    let mut opts = match &args.view {
        Some(v) => view::ViewOptions::load(v)?,
        None => match gidio::model_path(&file).map(|m| view::gv_path_for(&m)).filter(|g| g.exists()) {
            Some(gv) => view::ViewOptions::load(&gv)?, // the model's own .gv view
            None => view::load_settings().unwrap_or_default(),
        },
    };
    opts.format = view::FORMAT.into();
    let mut session = session::Session::new(opts);
    session.load(&file, &mut renderer)?;
    let (w, h) = args.size.unwrap_or((1600, 1000));
    let fmt = session::format_of(&out);
    session.export(&mut renderer, &out, &fmt, w, h, args.transparent)?;
    println!("wrote {}", out.display());
    Ok(())
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let batch_mode = args.export.is_some();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size(if batch_mode { [64.0, 64.0] } else { [1300.0, 800.0] })
            .with_visible(!batch_mode)
            .with_title("Postmesh"),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Postmesh",
        options,
        Box::new(move |cc| {
            if batch_mode {
                let gl = cc.gl.clone().expect("the OpenGL (glow) backend is required");
                match batch(&gl, args) {
                    Ok(()) => std::process::exit(0),
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        std::process::exit(1);
                    }
                }
            }
            let mut a = app::App::new(cc, args.file, args.view);
            a.gui_screenshot = args.gui_screenshot;
            Ok(Box::new(a))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

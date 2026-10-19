//! View options: persisted between sessions and exportable/importable as JSON.
//! The JSON layout is identical to the one written by the original Python version.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "postmesh-view";
/// format tag written by earlier versions (the app was called "GiD Replacer")
const LEGACY_FORMAT: &str = "gid-replacer-view";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ViewOptions {
    pub format: String,
    pub version: u32,
    pub show_deformation: bool,
    pub auto_scale: bool,
    pub show_edges: bool,
    /// outline / sharp edges of the shape (not the full mesh)
    pub show_outline: bool,
    pub show_undeformed: bool,
    pub show_legend: bool,
    pub show_caption: bool,
    pub show_axes: bool,
    pub show_toolbar: bool,
    pub fixed_range: bool,
    pub scale_factor: f64,
    pub range_min: f64,
    pub range_max: f64,
    pub colormap: String,
    pub result: String,
    pub component: String,
    pub step: String,
    pub deform_with: String,
    /// [position, focal point, view up]
    pub camera: Option<[[f64; 3]; 3]>,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            format: FORMAT.into(),
            version: 1,
            show_deformation: true,
            auto_scale: true,
            show_edges: false,
            show_outline: false,
            show_undeformed: true,
            show_legend: true,
            show_caption: true,
            show_axes: true,
            show_toolbar: true,
            fixed_range: false,
            scale_factor: 1.0,
            range_min: 0.0,
            range_max: 1.0,
            colormap: crate::colormap::NAMES[0].into(),
            result: String::new(),
            component: String::new(),
            step: String::new(),
            deform_with: String::new(),
            camera: None,
        }
    }
}

impl ViewOptions {
    pub fn load(path: &Path) -> Result<Self> {
        let txt = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let v: ViewOptions = serde_json::from_str(&txt).context("invalid view file")?;
        if v.format != FORMAT && v.format != LEGACY_FORMAT {
            bail!("not a Postmesh view file");
        }
        Ok(v)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// Per-user settings folder. Settings of the former name ("GidReplacer") are copied over once.
fn config_dir() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    let new = base.join("Postmesh");
    let old = base.join("GidReplacer");
    if !new.exists() && old.is_dir() {
        let _ = std::fs::create_dir_all(&new);
        for f in ["settings.json", "preferences.json"] {
            let _ = std::fs::copy(old.join(f), new.join(f));
        }
    }
    Some(new)
}

/// Location of the persistent settings file (per user).
pub fn settings_path() -> Option<PathBuf> {
    Some(config_dir()?.join("settings.json"))
}

/// Last-used options. The camera is deliberately not taken from here: it only makes sense
/// for the model it was saved with (that is what the per-model `.gv` file is for).
pub fn load_settings() -> Option<ViewOptions> {
    let mut v = ViewOptions::load(&settings_path()?).ok()?;
    v.camera = None;
    Some(v)
}

pub fn save_settings(v: &ViewOptions) {
    if let Some(p) = settings_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = v.save(&p);
    }
}

/// Application preferences (Edit ▸ Options).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Preferences {
    /// create / keep updating a `.gv` view file next to each model
    pub save_gv: bool,
    /// restore the view from the model's `.gv` file when it exists
    pub restore_gv: bool,
    /// last image-export choices
    pub export_format: String,
    /// 0 = follow the current view size (x2)
    pub export_width: u32,
    pub export_height: u32,
    pub export_transparent: bool,
    pub export_dir: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            save_gv: true,
            restore_gv: true,
            export_format: "png".into(),
            export_width: 0,
            export_height: 0,
            export_transparent: false,
            export_dir: String::new(),
        }
    }
}

fn prefs_path() -> Option<PathBuf> {
    Some(config_dir()?.join("preferences.json"))
}

impl Preferences {
    pub fn load() -> Self {
        prefs_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = prefs_path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(t) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, t);
            }
        }
    }
}

/// `<model>.gv` next to the `.msh` (same name, `.msh` replaced by `.gv`).
pub fn gv_path_for(msh: &Path) -> PathBuf {
    msh.with_extension("gv")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_roundtrip_and_old_files() {
        // an older preferences file without the export fields still loads
        let old: Preferences = serde_json::from_str(r#"{"save_gv": false}"#).unwrap();
        assert!(!old.save_gv && old.restore_gv && old.export_format == "png" && old.export_width == 0);
        let p = Preferences { export_format: "pdf".into(), export_width: 3200, export_height: 2000, export_transparent: true, export_dir: "/tmp".into(), ..old };
        let back: Preferences = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);
    }
}

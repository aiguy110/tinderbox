//! The panel's settings, saved between runs: in localStorage on the web (the
//! same key and field names the TypeScript app used), and in
//! `$XDG_CONFIG_HOME/tinderbox/params.json` natively.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Renderer {
    /// The procedural scene (the default).
    Scene,
    Flat,
}

// Saved settings may name a renderer that has since been removed or renamed
// ('dual' became 'tiles', and the tiles renderer didn't survive the port).
impl From<String> for Renderer {
    fn from(s: String) -> Self {
        if s == "flat" { Renderer::Flat } else { Renderer::Scene }
    }
}

impl From<Renderer> for String {
    fn from(r: Renderer) -> Self {
        match r {
            Renderer::Scene => "scene".into(),
            Renderer::Flat => "flat".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Params {
    /// Cell edge length in logical (CSS) pixels.
    pub cell_size: f32,
    /// p: probability per step that an empty cell grows a tree.
    pub growth: f32,
    /// f: probability per step that a tree is struck by lightning.
    pub lightning: f32,
    /// Derive lightning from growth and the grid size, on the curve where
    /// fire sizes are closest to a power law (see critical.rs).
    pub critical: bool,
    /// Simulation steps per second.
    pub steps_per_second: f32,
    /// Tree density used when (re)seeding.
    pub initial_density: f32,
    /// 8-neighbor (Moore) fire spread instead of 4-neighbor (von Neumann).
    pub moore: bool,
    /// Toroidal edges.
    pub wrap: bool,
    pub grid_lines: bool,
    /// Steps for burnt ground to green back over. Saved under a new key since
    /// the default doubled, so old saves pick up the new default.
    #[serde(rename = "regrowSteps")]
    pub regrow: f32,
    /// Scene renderer: average steps a snag stands after its cell clears.
    pub snag: f32,
    pub renderer: Renderer,
    /// Scene renderer: trees per cell side.
    pub trees: f32,
    /// Scene renderer: moonlight instead of daylight.
    pub night: bool,
    #[serde(skip_serializing)]
    pub paused: bool,
    pub panel_hidden: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            cell_size: 3.0,
            growth: 0.01,
            lightning: 0.00001,
            critical: false,
            steps_per_second: 60.0,
            initial_density: 0.5,
            moore: false,
            wrap: true,
            grid_lines: true,
            regrow: 20.0,
            snag: 10.0,
            renderer: Renderer::Scene,
            trees: 4.0,
            night: false,
            paused: false,
            panel_hidden: false,
        }
    }
}

impl Params {
    pub fn load() -> Self {
        let mut p: Params = storage::read().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        p.paused = false;
        p
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string(self) {
            storage::write(&s);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod storage {
    const KEY: &str = "tinderbox:params";

    fn local() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn read() -> Option<String> {
        local()?.get_item(KEY).ok()?
    }

    pub fn write(s: &str) {
        if let Some(l) = local() {
            let _ = l.set_item(KEY, s);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod storage {
    use std::path::PathBuf;

    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("tinderbox").join("params.json"))
    }

    pub fn read() -> Option<String> {
        std::fs::read_to_string(path()?).ok()
    }

    pub fn write(s: &str) {
        if let Some(p) = path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, s);
        }
    }
}

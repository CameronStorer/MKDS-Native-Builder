//! Display settings shared by the game window and the builder's menu: `settings.txt` beside `controls.txt` in the
//! user's settings folder. Command-line options of the game override it for one run.
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renderer {
    /// 3D and screen composition on the graphics card (block 2).
    Gpu,
    /// The CPU renderer (block 1); also the reference the GPU renderer is tested against.
    Software,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Top screen above the bottom screen, like the console.
    Stacked,
    /// Top screen left, bottom screen right (wide monitors).
    SideBySide,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub renderer: Renderer,
    /// Internal resolution as a multiple of 256x192 (1-8); `None` = the sharpest that fits the window.
    pub scale: Option<u32>,
    /// Linear filtering when the screens are stretched (`false` = sharp pixels).
    pub smooth: bool,
    pub layout: Layout,
    pub fullscreen: bool,
    /// Folder of replacement textures (`<id>.png`).
    pub texture_pack: Option<PathBuf>,
    /// Folder to save every texture the game uses into.
    pub dump_textures: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            renderer: Renderer::Gpu,
            scale: None,
            smooth: false,
            layout: Layout::Stacked,
            fullscreen: false,
            texture_pack: None,
            dump_textures: None,
        }
    }
}

fn yes_no(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" => Some(true),
        "no" | "off" | "false" => Some(false),
        _ => None,
    }
}

impl Settings {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut s = Settings::default();
        for (n, line) in text.lines().enumerate() {
            let (n, line) = (n + 1, line.split('#').next().unwrap_or("").trim());
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| format!("line {n}: expected `Name = value`"))?;
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            let bad = |what: &str| format!("line {n}: {what}, not `{value}`");
            match key.as_str() {
                "renderer" => {
                    s.renderer = match value.to_ascii_lowercase().as_str() {
                        "gpu" => Renderer::Gpu,
                        "software" => Renderer::Software,
                        _ => return Err(bad("Renderer must be GPU or Software")),
                    }
                }
                "scale" => {
                    s.scale = if value.eq_ignore_ascii_case("auto") {
                        None
                    } else {
                        Some(value.parse().ok().filter(|v| (1..=8).contains(v)).ok_or_else(|| bad("Scale must be 1-8 or Auto"))?)
                    }
                }
                "filter" => {
                    s.smooth = match value.to_ascii_lowercase().as_str() {
                        "sharp" => false,
                        "smooth" => true,
                        _ => return Err(bad("Filter must be Sharp or Smooth")),
                    }
                }
                "layout" => {
                    s.layout = match value.to_ascii_lowercase().as_str() {
                        "stacked" => Layout::Stacked,
                        "sidebyside" | "side-by-side" | "side by side" => Layout::SideBySide,
                        _ => return Err(bad("Layout must be Stacked or SideBySide")),
                    }
                }
                "fullscreen" => s.fullscreen = yes_no(value).ok_or_else(|| bad("Fullscreen must be Yes or No"))?,
                "texturepack" => s.texture_pack = (!value.is_empty()).then(|| PathBuf::from(value)),
                "dumptextures" => s.dump_textures = (!value.is_empty()).then(|| PathBuf::from(value)),
                _ => return Err(format!("line {n}: unknown setting `{}`", line.split('=').next().unwrap_or("").trim())),
            }
        }
        Ok(s)
    }

    pub fn to_text(&self) -> String {
        let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        format!(
            "# MKDS Native Builder display settings. Edit here or with mkds.bat -> \"Display settings\".\n\
             # The game's command-line options (--renderer, --scale) override these for one run.\n\n\
             Renderer = {}              # GPU or Software\n\
             Scale = {}                 # 1-8 (x the DS's 256x192), or Auto: the sharpest that fits the window\n\
             Filter = {}             # Sharp or Smooth, when the screens are stretched\n\
             Layout = {}           # Stacked (top over bottom) or SideBySide\n\
             Fullscreen = {}            # Yes or No (F11 switches while playing)\n\
             TexturePack = {}            # folder of replacement textures (<id>.png); empty = none\n\
             DumpTextures = {}           # folder to save the game's textures into; empty = off\n",
            if self.renderer == Renderer::Gpu { "GPU" } else { "Software" },
            self.scale.map_or("Auto".to_string(), |s| s.to_string()),
            if self.smooth { "Smooth" } else { "Sharp" },
            if self.layout == Layout::Stacked { "Stacked" } else { "SideBySide" },
            if self.fullscreen { "Yes" } else { "No" },
            path(&self.texture_pack),
            path(&self.dump_textures),
        )
    }

    /// Reads `path`, writing the defaults there first when it does not exist yet.
    pub fn load_or_create(path: &std::path::Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = Self::default();
                s.save(path)?;
                Ok(s)
            }
            Err(e) => Err(format!("Cannot read {}: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
        std::fs::write(path, self.to_text()).map_err(|e| format!("Cannot write {}: {e}", path.display()))
    }
}

/// `settings.txt` in the same folder as `controls.txt`.
pub fn default_path() -> Option<PathBuf> {
    crate::controls::default_path().map(|p| p.with_file_name("settings.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_and_values_parse() {
        let d = Settings::default();
        assert_eq!(Settings::parse(&d.to_text()).unwrap(), d);
        let s = Settings::parse("renderer = software\nScale=3\nfilter = SMOOTH\nLayout = SideBySide\nFullscreen = yes\nTexturePack = C:\\packs\\hd\n").unwrap();
        assert_eq!(s.renderer, Renderer::Software);
        assert_eq!(s.scale, Some(3));
        assert!(s.smooth && s.fullscreen);
        assert_eq!(s.layout, Layout::SideBySide);
        assert_eq!(s.texture_pack, Some(PathBuf::from("C:\\packs\\hd")));
        assert_eq!(Settings::parse(&s.to_text()).unwrap(), s);
        assert_eq!(Settings::parse("Scale = auto").unwrap().scale, None);
    }

    #[test]
    fn mistakes_name_the_line() {
        for (text, expect) in [
            ("Scale = 9", "line 1: Scale must be 1-8 or Auto"),
            ("Renderer = gpu\nRenderer = vulkan", "line 2: Renderer must be GPU or Software"),
            ("Brightness = 3", "line 1: unknown setting `Brightness`"),
            ("Fullscreen", "line 1: expected"),
        ] {
            let e = Settings::parse(text).unwrap_err();
            assert!(e.starts_with(expect), "{e}");
        }
    }
}

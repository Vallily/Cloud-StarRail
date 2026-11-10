//! Versioned preferences live in the user's application data, never beside the EXE.
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tauri::{Manager, Window};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Shortcut {
    pub key: u32,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
}
impl Default for Shortcut {
    fn default() -> Self {
        Self {
            key: 0x7b,
            ctrl: false,
            shift: false,
            alt: false,
            win: false,
        }
    }
}
impl Shortcut {
    pub fn valid(&self) -> bool {
        matches!(self.key, 0x30..=0x39 | 0x41..=0x5a | 0x70..=0x87 | 0x20 | 0x21..=0x28 | 0x2c..=0x2e | 0xba..=0xc0 | 0xdb..=0xde)
            && !(self.alt && self.key == 0x73)
            && self.key != 0x7a
            && !self.win
            && !(self.alt && self.key == 0x09)
            && !(self.ctrl && matches!(self.key, 0x1b | 0x2e))
    }
    pub fn matches(&self, key: u32) -> bool {
        self.key == key && self.modifiers()
    }
    pub fn modifiers(&self) -> bool {
        unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState as down;
            self.ctrl == (down(0x11) < 0)
                && self.shift == (down(0x10) < 0)
                && self.alt == (down(0x12) < 0)
                && self.win == (down(0x5b) < 0 || down(0x5c) < 0)
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub folder: PathBuf,
    pub quality: String,
    pub resolution: u32,
    pub custom_mbps: f64,
    pub screenshot: String,
    pub record_key: Shortcut,
    pub screenshot_key: Shortcut,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            folder: PathBuf::new(),
            quality: "high".into(),
            resolution: 1080,
            custom_mbps: 12.,
            screenshot: "compressed".into(),
            record_key: Shortcut {
                ctrl: true,
                ..Default::default()
            },
            screenshot_key: Shortcut::default(),
        }
    }
}
pub fn bounds(height: u32) -> [f64; 4] {
    match height {
        2160.. => [19., 28.5, 38., 50.],
        1440.. => [11., 16.5, 22., 32.],
        1080.. => [6., 9., 12., 24.],
        _ => [3.75, 5.6, 7.5, 12.],
    }
}
impl Settings {
    pub fn bitrate(&self, height: u32) -> u32 {
        let b = bounds(height);
        let value = match self.quality.as_str() {
            "low" => b[0],
            "medium" => b[1],
            "ultra" => b[3],
            "custom" => self.custom_mbps.clamp(b[0], b[3]),
            _ => b[2],
        };
        (value * 1_000_000.).round() as u32
    }
    pub fn validate(&mut self) -> Result<(), String> {
        if self.folder.as_os_str().is_empty() || !self.folder.is_absolute() {
            return Err("请选择有效的绝对保存路径".into());
        }
        if ![0, 720, 1080, 1440, 2160].contains(&self.resolution)
            || !["low", "medium", "high", "ultra", "custom"].contains(&self.quality.as_str())
            || !["compressed", "original", "both"].contains(&self.screenshot.as_str())
            || !self.custom_mbps.is_finite()
        {
            return Err("配置值无效".into());
        }
        if !self.record_key.valid() || !self.screenshot_key.valid() {
            return Err("该快捷键不可用，请避开系统和窗口快捷键".into());
        }
        if self.record_key == self.screenshot_key {
            return Err("截图与录屏不能使用相同的快捷键".into());
        }
        std::fs::create_dir_all(&self.folder).map_err(|e| format!("无法创建保存目录：{e}"))?;
        Ok(())
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Placement {
    pub fullscreen: bool,
    pub width: f64,
    pub x: i32,
    pub y: i32,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Document {
    pub schema: u32,
    pub settings: Settings,
    pub window: Option<Placement>,
}
pub struct Preferences {
    pub data: Mutex<Document>,
    path: PathBuf,
    pub editing: AtomicBool,
}
impl Preferences {
    pub fn load(app: &tauri::AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let dir = app.path().app_config_dir()?;
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("preferences.json");
        let existing = std::fs::read(&path).ok();
        let mut data = match existing {
            Some(bytes) => match serde_json::from_slice::<Document>(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Invalid preferences: {e}");
                    std::fs::copy(&path, dir.join("preferences.invalid.json"))?;
                    Document::default()
                }
            },
            None => Document::default(),
        };
        if data.settings.folder.as_os_str().is_empty() {
            data.settings.folder = std::env::current_exe()?
                .parent()
                .ok_or("Missing executable directory")?
                .join("cloud-starrail-screencapture");
            if let Err(e) = std::fs::create_dir_all(&data.settings.folder) {
                eprintln!("Cannot create default screenshot folder: {e}");
            }
        }
        // Treat invalid individual values as a damaged configuration, not an input
        // index or encoder parameter. Keep a user's offline save directory intact.
        if !data.settings.record_key.valid()
            || !data.settings.screenshot_key.valid()
            || data.settings.record_key == data.settings.screenshot_key
        {
            data.settings.record_key = Settings::default().record_key;
            data.settings.screenshot_key = Shortcut::default();
        }
        if ![720, 1080, 1440, 2160].contains(&data.settings.resolution) {
            data.settings.resolution = 1080;
        }
        if !["low", "medium", "high", "ultra", "custom"].contains(&data.settings.quality.as_str()) {
            data.settings.quality = "high".into();
        }
        if !["compressed", "original", "both"].contains(&data.settings.screenshot.as_str()) {
            data.settings.screenshot = "compressed".into();
        }
        if !data.settings.custom_mbps.is_finite() {
            data.settings.custom_mbps = 12.;
        }
        if data.schema == 0 {
            let point = app.cursor_position()?;
            if let Some(m) = app
                .monitor_from_point(point.x, point.y)?
                .or(app.primary_monitor()?)
            {
                data.settings.resolution = match m.size().height {
                    2160.. => 2160,
                    1440.. => 1440,
                    1080.. => 1080,
                    _ => 720,
                };
            }
            data.schema = 1;
        }
        Ok(Self {
            data: Mutex::new(data),
            path,
            editing: AtomicBool::new(false),
        })
    }
    pub fn settings(&self) -> Settings {
        self.data.lock().unwrap().settings.clone()
    }
    pub fn commit(&self, data: &Document) -> Result<(), String> {
        let temp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(data).map_err(|e| e.to_string())?;
        use std::io::Write;
        let mut f = std::fs::File::create(&temp).map_err(|e| e.to_string())?;
        f.write_all(&bytes)
            .and_then(|()| f.sync_all())
            .map_err(|e| e.to_string())?;
        drop(f);
        std::fs::rename(temp, &self.path).map_err(|e| e.to_string())
    }
    pub fn save_window(&self, w: &Window) {
        let fullscreen = w.is_fullscreen().unwrap_or(false);
        let mut data = self.data.lock().unwrap();
        let old = data.window.clone().unwrap_or_default();
        data.window = Some(if fullscreen || w.is_minimized().unwrap_or(false) {
            Placement { fullscreen, ..old }
        } else {
            let scale = w.scale_factor().unwrap_or(1.);
            let size = w.inner_size().ok();
            let pos = w.outer_position().ok();
            Placement {
                fullscreen,
                width: size.map(|s| s.width as f64 / scale).unwrap_or(old.width),
                x: pos.map(|p| p.x).unwrap_or(old.x),
                y: pos.map(|p| p.y).unwrap_or(old.y),
            }
        });
        if let Err(e) = self.commit(&data) {
            eprintln!("Cannot persist window: {e}");
        }
    }
    pub fn editing(&self) -> bool {
        self.editing.load(Ordering::Relaxed)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_replace_preserves_settings_and_window() {
        let name = format!(
            "cloud-starrail-prefs-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(name);
        let prefs = Preferences {
            data: Mutex::new(Document::default()),
            path: path.clone(),
            editing: AtomicBool::new(false),
        };
        let mut doc = Document {
            schema: 1,
            window: Some(Placement {
                fullscreen: true,
                width: 1280.,
                x: 30,
                y: 40,
            }),
            ..Default::default()
        };
        prefs.commit(&doc).unwrap();
        doc.settings.quality = "ultra".into();
        doc.settings.record_key.shift = true;
        prefs.commit(&doc).unwrap();
        let restored: Document = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored.settings.quality, "ultra");
        assert!(restored.settings.record_key.shift);
        assert!(restored.window.unwrap().fullscreen);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn tiers_and_shortcuts() {
        let mut s = Settings::default();
        assert_eq!(s.bitrate(1080), 12_000_000);
        s.quality = "custom".into();
        s.custom_mbps = 100.;
        assert_eq!(s.bitrate(720), 12_000_000);
        assert!(!Shortcut {
            key: 0x7a,
            ..Default::default()
        }
        .valid());
        assert_ne!(s.record_key, s.screenshot_key);
    }
}

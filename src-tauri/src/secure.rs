use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn try_dir(p: &PathBuf) -> bool {
    std::fs::create_dir_all(p).is_ok() && std::fs::write(p.join(".w"), b"1").is_ok()
}

/// 应用数据目录：优先 %APPDATA%，不可写时回退到 exe 同级 data\ 或 %TEMP%。
pub fn data_dir() -> PathBuf {
    DATA_DIR
        .get_or_init(|| {
            let mut candidates: Vec<PathBuf> = Vec::new();
            if let Ok(a) = std::env::var("APPDATA") {
                candidates.push(PathBuf::from(a).join("PhuketBuddy"));
            }
            if let Some(d) = exe_dir() {
                candidates.push(d.join("data"));
            }
            if let Ok(t) = std::env::var("TEMP") {
                candidates.push(PathBuf::from(t).join("PhuketBuddy"));
            }
            for c in candidates {
                if try_dir(&c) {
                    let _ = std::fs::remove_file(c.join(".w"));
                    return c;
                }
            }
            PathBuf::from(".")
        })
        .clone()
}

/// WebView2 用户数据目录：同样带回退，避免受限环境下窗口创建失败。
pub fn webview_data_dir() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(l).join("PhuketBuddy").join("webview"));
    }
    if let Some(d) = exe_dir() {
        candidates.push(d.join("data").join("webview"));
    }
    if let Ok(t) = std::env::var("TEMP") {
        candidates.push(PathBuf::from(t).join("PhuketBuddy").join("webview"));
    }
    for c in candidates {
        if try_dir(&c) {
            let _ = std::fs::remove_file(c.join(".w"));
            return c;
        }
    }
    std::env::temp_dir().join("PhuketBuddy").join("webview")
}

/* ---------- DPAPI 加密 ---------- */
mod dpapi {
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let input = blob(data);
            let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
            let ok = CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 || out.pbData.is_null() {
                return None;
            }
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as HLOCAL);
            Some(v)
        }
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let input = blob(data);
            let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
            let ok = CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 || out.pbData.is_null() {
                return None;
            }
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as HLOCAL);
            Some(v)
        }
    }
}

pub fn save_session(session: &crate::api::Session) {
    let Ok(raw) = serde_json::to_vec(session) else { return };
    let Some(enc) = dpapi::protect(&raw) else { return };
    let _ = std::fs::write(data_dir().join("session.bin"), enc);
}

pub fn load_session() -> Option<crate::api::Session> {
    let enc = std::fs::read(data_dir().join("session.bin")).ok()?;
    let raw = dpapi::unprotect(&enc)?;
    serde_json::from_slice(&raw).ok()
}

pub fn clear_session() {
    let _ = std::fs::remove_file(data_dir().join("session.bin"));
}

/* ---------- DeepSeek API Key（DPAPI 加密） ---------- */
pub fn save_ds_key(key: &str) -> bool {
    match dpapi::protect(key.trim().as_bytes()) {
        Some(enc) => std::fs::write(data_dir().join("deepseek.bin"), enc).is_ok(),
        None => false,
    }
}

pub fn load_ds_key() -> Option<String> {
    let enc = std::fs::read(data_dir().join("deepseek.bin")).ok()?;
    let raw = dpapi::unprotect(&enc)?;
    String::from_utf8(raw).ok().map(|s| s.trim().to_string())
}

pub fn clear_ds_key() {
    let _ = std::fs::remove_file(data_dir().join("deepseek.bin"));
    let _ = std::fs::remove_file(data_dir().join("deepseek.json"));
}

pub fn save_ds_cache(v: &Value) {
    if let Ok(b) = serde_json::to_vec(v) {
        let _ = std::fs::write(data_dir().join("deepseek.json"), b);
    }
}

pub fn load_ds_cache() -> Option<Value> {
    let b = std::fs::read(data_dir().join("deepseek.json")).ok()?;
    serde_json::from_slice(&b).ok()
}

/* ---------- 设置 ---------- */
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Settings {
    pub refresh_minutes: u32,
    pub autostart: bool,
    pub alert_threshold: f64,
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 启动时只驻留托盘，不显示气泡
    #[serde(default)]
    pub start_hidden: bool,
    /// 气泡显示哪个来源的余额：phuket / deepseek
    #[serde(default = "default_source")]
    pub bubble_source: String,
}

fn default_source() -> String {
    "phuket".to_string()
}

fn default_theme() -> String {
    "dark".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_minutes: 5,
            autostart: false,
            alert_threshold: 1.0,
            theme: default_theme(),
            start_hidden: false,
            bubble_source: default_source(),
        }
    }
}

const SETTINGS_FILE: &str = "settings.json";

pub fn load_settings() -> Settings {
    std::fs::read(data_dir().join(SETTINGS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    if let Ok(b) = serde_json::to_vec_pretty(s) {
        let _ = std::fs::write(data_dir().join(SETTINGS_FILE), b);
    }
}

/* ---------- 缓存 ---------- */
pub fn save_cache(v: &Value) {
    if let Ok(b) = serde_json::to_vec(v) {
        let _ = std::fs::write(data_dir().join("cache.json"), b);
    }
}

pub fn load_cache() -> Option<Value> {
    let b = std::fs::read(data_dir().join("cache.json")).ok()?;
    serde_json::from_slice(&b).ok()
}

/* ---------- 诊断日志 ---------- */
pub fn log_line(msg: &str) {
    use std::io::Write;
    let path = data_dir().join("error.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

/* ---------- 诊断：只落结构，不落数值 ---------- */
pub fn save_raw(v: &Value) {
    if let Ok(b) = serde_json::to_vec_pretty(v) {
        let _ = std::fs::write(data_dir().join("raw-shape.json"), b);
    }
}

/* ---------- 开机自启 ----------
 * 先尝试注册表 HKCU\...\Run；失败则回退到「启动」文件夹放一个 .cmd。
 * 返回 Ok("registry") / Ok("startup") 表示用了哪种方式。
 */

fn registry_set(enabled: bool) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ,
    };

    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
        .encode_utf16()
        .collect();
    let value: Vec<u16> = "PhuketBuddy\0".encode_utf16().collect();

    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let rc = RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_SET_VALUE, &mut hkey);
        if rc != 0 {
            return Err(format!("打开注册表键失败（错误码 {rc}）"));
        }

        let rc = if enabled {
            match std::env::current_exe() {
                Ok(exe) => {
                    let cmd = format!("\"{}\"", exe.display());
                    let data: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
                    RegSetValueExW(
                        hkey,
                        value.as_ptr(),
                        0,
                        REG_SZ,
                        data.as_ptr() as *const u8,
                        (data.len() * 2) as u32,
                    )
                }
                Err(e) => {
                    RegCloseKey(hkey);
                    return Err(format!("取可执行文件路径失败：{e}"));
                }
            }
        } else {
            RegDeleteValueW(hkey, value.as_ptr())
        };

        RegCloseKey(hkey);

        // 2 = ERROR_FILE_NOT_FOUND：删除一个本来就不存在的值，视为成功
        if rc != 0 && !(rc == 2 && !enabled) {
            return Err(format!("写入注册表失败（错误码 {rc}）"));
        }
    }
    Ok(())
}

fn startup_shortcut(enabled: bool) -> Result<(), String> {
    let appdata = std::env::var("APPDATA").map_err(|_| "找不到 APPDATA".to_string())?;
    let dir = std::path::PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup");
    let file = dir.join("PhuketBuddy.cmd");
    if !enabled {
        if file.exists() {
            let _ = std::fs::remove_file(&file);
        }
        return Ok(());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建启动文件夹失败：{e}"))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let body = format!("@echo off\r\nstart \"\" \"{}\"\r\n", exe.display());
    std::fs::write(&file, body).map_err(|e| format!("写入启动项失败：{e}"))
}

pub fn set_autostart(enabled: bool) -> Result<&'static str, String> {
    if !enabled {
        let _ = registry_set(false);
        let _ = startup_shortcut(false);
        return Ok("disabled");
    }

    match registry_set(true) {
        Ok(_) => {
            let _ = startup_shortcut(false);
            Ok("registry")
        }
        Err(reg_err) => match startup_shortcut(true) {
            Ok(_) => {
                log_line(&format!("autostart: registry failed ({reg_err}), used Startup folder"));
                Ok("startup")
            }
            Err(startup_err) => Err(format!("{reg_err}；启动文件夹也不行：{startup_err}（当前进程可能是受限权限，试试从资源管理器直接双击 exe 后再开）")),
        },
    }
}

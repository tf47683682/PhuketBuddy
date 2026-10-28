mod api;
mod secure;
mod win;

use api::{now_secs, Api, Session};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, PhysicalSize, WebviewWindow};

pub struct AppState {
    api: Api,
    session: Mutex<Option<Session>>,
    settings: Mutex<secure::Settings>,
    last_blur: Mutex<Option<Instant>>,
    panel_shown_at: Mutex<Option<Instant>>,
    dock: Mutex<Option<win::Edge>>,
    dock_hidden: Mutex<bool>,
    hovered: Mutex<bool>,
    pinned: Mutex<bool>,
}

/* ------------------------- 贴边半隐藏 ------------------------- */

fn dock_show(app: &AppHandle) {
    let st = app.state::<AppState>();
    let edge = *st.dock.lock().unwrap();
    if let (Some(e), Some(w)) = (edge, app.get_webview_window("bubble")) {
        win::apply_dock(&w, e, false);
        *st.dock_hidden.lock().unwrap() = false;
    }
}

fn dock_hide_now(app: &AppHandle) -> bool {
    let st = app.state::<AppState>();
    if *st.pinned.lock().unwrap() || *st.hovered.lock().unwrap() {
        return false;
    }
    let edge = *st.dock.lock().unwrap();
    if let (Some(e), Some(w)) = (edge, app.get_webview_window("bubble")) {
        win::apply_dock(&w, e, true);
        *st.dock_hidden.lock().unwrap() = true;
        return true;
    }
    false
}

fn schedule_dock_hide(app: AppHandle, delay_ms: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        dock_hide_now(&app);
    });
}

fn refresh_dock(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("bubble") {
        if let Some(e) = win::nearest_edge(&w) {
            let st = app.state::<AppState>();
            *st.dock.lock().unwrap() = Some(e);
        }
    }
}

#[tauri::command]
fn bubble_hover(app: AppHandle, inside: bool) {
    let st = app.state::<AppState>();
    *st.hovered.lock().unwrap() = inside;
    if inside {
        dock_show(&app);
    } else {
        schedule_dock_hide(app.clone(), 650);
    }
}

/* ------------------------- 鉴权与取数 ------------------------- */

async fn ensure_access(state: &AppState) -> Result<String, String> {
    let (access, refresh, exp) = {
        let g = state.session.lock().map_err(|_| "状态锁异常")?;
        let s = g.as_ref().ok_or("未登录")?;
        (s.access.clone(), s.refresh.clone(), s.expires_at)
    };
    if exp - 60 > now_secs() {
        return Ok(access);
    }
    if refresh.is_empty() {
        return Err("登录状态已失效，请重新登录".into());
    }
    let ns = state.api.refresh(&refresh).await.map_err(|e| e.to_string())?;
    store_session(state, ns.clone());
    Ok(ns.access)
}

fn store_session(state: &AppState, mut ns: Session) {
    {
        if let Ok(g) = state.session.lock() {
            if let Some(old) = g.as_ref() {
                if ns.user.is_null() {
                    ns.user = old.user.clone();
                }
            }
        }
    }
    secure::save_session(&ns);
    if let Ok(mut g) = state.session.lock() {
        *g = Some(ns);
    }
}

async fn get_json(state: &AppState, path: &str) -> Result<Value, String> {
    let access = ensure_access(state).await?;
    match state.api.request(reqwest::Method::GET, path, None, Some(&access)).await {
        Ok(v) => Ok(v),
        Err(api::ApiError::Unauthorized) => {
            let refresh = state
                .session
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|s| s.refresh.clone()))
                .unwrap_or_default();
            if refresh.is_empty() {
                return Err("登录状态已失效".into());
            }
            let ns = state
                .api
                .refresh(&refresh)
                .await
                .map_err(|_| "登录状态已失效，请重新登录".to_string())?;
            store_session(state, ns.clone());
            state
                .api
                .request(reqwest::Method::GET, path, None, Some(&ns.access))
                .await
                .map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

async fn load_dashboard(state: &AppState) -> Result<Value, String> {
    // 三个接口各自容错：任一失败都不应让整个面板变空
    let me_res = get_json(state, "/auth/me").await;
    let stats_res = get_json(state, "/usage/dashboard/stats").await;

    if me_res.is_err() && stats_res.is_err() {
        return Err(stats_res.err().unwrap_or_else(|| "数据获取失败".into()));
    }
    let me = me_res.ok();
    let stats = stats_res.unwrap_or(Value::Null);

    let from = api::ymd_offset(-6);
    let to = api::ymd_offset(0);
    let trend = get_json(
        state,
        &format!(
            "/usage/dashboard/trend?start_date={}&end_date={}&granularity=day",
            from, to
        ),
    )
    .await
    .unwrap_or(Value::Null);
    let keys = get_json(state, "/keys").await.unwrap_or(Value::Null);

    let user = me
        .as_ref()
        .map(|v| api::user_from(api::unwrap_data(v)))
        .unwrap_or(Value::Null);
    let stats_n = api::normalize_stats(&stats);
    let balance = user
        .get("balance")
        .and_then(|b| b.as_f64())
        .or_else(|| stats_n.get("balance").and_then(|b| b.as_f64()));

    // 诊断用：只记录字段结构，不记录任何数值/密钥
    secure::save_raw(&json!({
        "me": me.as_ref().map(api::shape_of),
        "stats": api::shape_of(&stats),
        "trend": api::shape_of(&trend),
        "keys": api::shape_of(&keys),
    }));

    Ok(json!({
        "user": user,
        "balance": balance,
        "stats": stats_n,
        "trend": api::normalize_trend(&trend),
        "keys": api::normalize_keys(&keys),
        "syncedAt": now_secs(),
        "range": { "from": from, "to": to },
    }))
}

/* ------------------------- 演示数据（PHUKETBUDDY_DEMO=1） ------------------------- */

fn demo_mode() -> bool {
    std::env::var("PHUKETBUDDY_DEMO").map(|v| v == "1").unwrap_or(false)
}

fn demo_dashboard() -> Value {
    let d = |off: i64| api::ymd_offset(off);
    json!({
        "user": {
            "id": 1,
            "email": "demo@example.com",
            "nickname": "demo",
            "balance": 86.75,
            "role": "default",
            "raw": {}
        },
        "balance": 86.75,
        "stats": {
            "todayCost": 4.32,
            "todayRequests": 1284,
            "totalCost": 41.18,
            "totalRequests": 9321,
            "keyCount": 3,
            "balance": 86.75,
            "byPlatform": Value::Null
        },
        "trend": [
            { "date": d(-6), "cost": 2.10, "requests": 421 },
            { "date": d(-5), "cost": 3.40, "requests": 602 },
            { "date": d(-4), "cost": 2.45, "requests": 511 },
            { "date": d(-3), "cost": 4.30, "requests": 780 },
            { "date": d(-2), "cost": 3.02, "requests": 640 },
            { "date": d(-1), "cost": 5.10, "requests": 902 },
            { "date": d(0),  "cost": 4.32, "requests": 1284 }
        ],
        "keys": [
            { "id": 1, "name": "生产环境", "key": "sk-pukitdemo1111111111111111111111abcd",
              "masked": "sk-puk****abcd", "quota": 62.10, "enabled": true,
              "statusText": "active", "expiresAt": "", "group": "default" },
            { "id": 2, "name": "本地调试", "key": "sk-pukitdemo2222222222222222222222ef01",
              "masked": "sk-puk****ef01", "quota": 24.65, "enabled": true,
              "statusText": "active", "expiresAt": "", "group": "default" },
            { "id": 3, "name": "临时测试", "key": "",
              "masked": "sk-puk****7e10", "quota": Value::Null, "enabled": false,
              "statusText": "expired", "expiresAt": "2026-09-30", "group": "default" }
        ],
        "syncedAt": now_secs(),
        "range": { "from": d(-6), "to": d(0) },
        "demo": true
    })
}

/* ------------------------- 命令 ------------------------- */

#[tauri::command]
async fn boot(app: AppHandle) -> Result<Value, String> {
    let state = app.state::<AppState>();
    let (logged, user) = {
        let g = state.session.lock().map_err(|_| "状态锁异常")?;
        match g.as_ref() {
            Some(s) => (true, s.user.clone()),
            None => (false, Value::Null),
        }
    };
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or_default();
    let demo = demo_mode();
    let dd = secure::data_dir();
    let restricted = std::env::var("APPDATA")
        .ok()
        .map(|a| !dd.to_string_lossy().starts_with(&a))
        .unwrap_or(true);
    Ok(json!({
        "loggedIn": logged || demo,
        "user": if demo { demo_dashboard()["user"].clone() } else { user },
        "settings": settings,
        "cache": if demo { demo_dashboard() } else { secure::load_cache().unwrap_or(Value::Null) },
        "version": app.package_info().version.to_string(),
        "demo": demo,
        "deepseek": ds_state(),
        "dataDir": dd.to_string_lossy().to_string(),
        "restricted": restricted,
    }))
}

#[tauri::command]
async fn login(app: AppHandle, email: String, password: String) -> Result<Value, String> {
    let state = app.state::<AppState>();
    let (mut session, _user) = state
        .api
        .login(email.trim(), &password)
        .await
        .map_err(|e| e.to_string())?;

    // 登录后再拉一次用户信息，补全余额等字段
    let mut user = api::user_from(&session.user);
    if let Ok(me) = state
        .api
        .request(reqwest::Method::GET, "/auth/me", None, Some(&session.access))
        .await
    {
        user = api::user_from(api::unwrap_data(&me));
    }
    session.user = user.clone();
    store_session(&state, session);
    let _ = app.emit("pb://session", json!({ "loggedIn": true, "user": user }));
    Ok(user)
}

#[tauri::command]
fn logout(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    if let Ok(mut g) = state.session.lock() {
        *g = None;
    }
    secure::clear_session();
    let _ = std::fs::remove_file(secure::data_dir().join("cache.json"));
    let _ = app.emit("pb://session", json!({ "loggedIn": false, "user": null }));
    Ok(())
}

#[tauri::command]
async fn dashboard(app: AppHandle) -> Result<Value, String> {
    if demo_mode() {
        let d = demo_dashboard();
        let _ = app.emit("pb://dashboard", d.clone());
        return Ok(d);
    }
    let state = app.state::<AppState>();
    let d = load_dashboard(&state).await?;
    secure::save_cache(&d);
    let _ = app.emit("pb://dashboard", d.clone());
    Ok(d)
}

#[tauri::command]
fn save_settings(app: AppHandle, mut settings: secure::Settings) -> Result<Value, String> {
    let state = app.state::<AppState>();
    let prev = state.settings.lock().map(|s| s.clone()).unwrap_or_default();

    // 自启失败不阻断其它设置保存；失败时把开关恢复成实际状态
    let mut warning = String::new();
    if prev.autostart != settings.autostart {
        match secure::set_autostart(settings.autostart) {
            Ok("startup") => {
                warning = "已保存；注册表被拦截，开机自启改用「启动」文件夹".to_string();
            }
            Ok(_) => {}
            Err(e) => {
                settings.autostart = prev.autostart;
                warning = format!("设置已保存，但开机自启没生效：{e}");
            }
        }
    }

    let theme_changed = prev.theme != settings.theme;
    let theme = settings.theme.clone();
    secure::save_settings(&settings);
    if let Ok(mut g) = state.settings.lock() {
        *g = settings.clone();
    }
    if theme_changed {
        let _ = app.emit("pb://theme", theme);
    }
    Ok(json!({
        "autostart": settings.autostart,
        "theme": settings.theme,
        "start_hidden": settings.start_hidden,
        "warning": warning,
    }))
}

#[tauri::command]
fn bubble_down(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    let start = window.outer_position().ok();
    if let Err(e) = window.start_dragging() {
        secure::log_line(&format!("drag: start_dragging err {e}"));
    }

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        let mut guard = 0;
        let mut last = window.outer_position().ok();
        let mut stable = 0;
        let mut released = 0;
        loop {
            std::thread::sleep(Duration::from_millis(25));
            guard += 1;
            let now = window.outer_position().ok();
            if now != last {
                last = now;
                stable = 0;
            } else {
                stable += 1;
            }
            if win::left_button_down() {
                released = 0;
            } else {
                released += 1;
            }
            // 按键已松开且位置稳定约 100ms，视为拖拽结束
            if released >= 3 && stable >= 4 {
                break;
            }
            if guard > 240 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
        let end = window.outer_position().ok();
        let moved = match (start, end) {
            (Some(a), Some(b)) => (a.x - b.x).abs() > 4 || (a.y - b.y).abs() > 4,
            _ => false,
        };
        if moved {
            win::snap(&window);
            // 拖动结束后重新记录贴边方向，并保持完整显示（鼠标还在上面）
            refresh_dock(&app);
            dock_show(&app);
        } else if guard <= 0 {
            secure::log_line("drag: watcher timed out");
        }
        let _ = app.emit("pb://drag-end", json!({ "moved": moved }));
    });
    Ok(())
}

#[tauri::command]
fn toggle_panel(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let panel = app.get_webview_window("panel").ok_or("面板窗口不存在")?;
    let bubble = app.get_webview_window("bubble").ok_or("气泡窗口不存在")?;

    if panel.is_visible().unwrap_or(false) {
        let _ = panel.hide();
        if let Ok(mut g) = state.pinned.lock() {
            *g = false;
        }
        schedule_dock_hide(app.clone(), 350);
        return Ok(());
    }
    {
        let mut blur = state.last_blur.lock().map_err(|_| "状态锁异常")?;
        if let Some(t) = *blur {
            if t.elapsed() < Duration::from_millis(320) {
                *blur = None;
                return Ok(());
            }
        }
    }
    // 面板打开期间气泡常显
    if let Ok(mut g) = state.pinned.lock() {
        *g = true;
    }
    dock_show(&app);
    secure::log_line(&format!(
        "toggle: bubble={:?} panel_before={:?}",
        bubble.outer_position(),
        panel.outer_position()
    ));
    win::place_panel(&panel, &bubble);
    secure::log_line(&format!("toggle: panel_after={:?}", panel.outer_position()));
    panel.show().map_err(|e| e.to_string())?;
    if let Ok(mut g) = state.panel_shown_at.lock() {
        *g = Some(Instant::now());
    }
    let _ = panel.set_focus();
    let _ = app.emit("pb://panel-shown", ());
    Ok(())
}

#[tauri::command]
fn hide_panel(app: AppHandle) -> Result<(), String> {
    secure::log_line("hide_panel command");
    if let Some(p) = app.get_webview_window("panel") {
        let _ = p.hide();
    }
    let state = app.state::<AppState>();
    if let Ok(mut g) = state.pinned.lock() {
        *g = false;
    }
    schedule_dock_hide(app.clone(), 300);
    Ok(())
}

#[tauri::command]
fn panel_resize(app: AppHandle, height: f64) -> Result<(), String> {
    let panel = app.get_webview_window("panel").ok_or("面板窗口不存在")?;
    let bubble = app.get_webview_window("bubble").ok_or("气泡窗口不存在")?;
    let scale = panel.scale_factor().unwrap_or(1.0);
    let w = (348.0 * scale).round() as u32;
    let h = (height.clamp(140.0, 760.0) * scale).round() as u32;
    panel
        .set_size(PhysicalSize::new(w, h))
        .map_err(|e| e.to_string())?;
    if panel.is_visible().unwrap_or(false) {
        win::place_panel(&panel, &bubble);
    }
    Ok(())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// 转入后台：隐藏气泡和面板，只留托盘图标；进程继续运行、定时刷新不停
#[tauri::command]
fn enter_background(app: AppHandle) -> Result<(), String> {
    if let Some(p) = app.get_webview_window("panel") {
        let _ = p.hide();
    }
    if let Some(b) = app.get_webview_window("bubble") {
        let _ = b.hide();
    }
    let st = app.state::<AppState>();
    if let Ok(mut g) = st.pinned.lock() {
        *g = false;
    }
    if let Ok(mut g) = st.hovered.lock() {
        *g = false;
    }
    secure::log_line("enter background (tray only)");
    Ok(())
}

#[tauri::command]
fn data_path() -> String {
    secure::data_dir().to_string_lossy().to_string()
}

#[tauri::command]
fn open_data_dir() {
    let p = secure::data_dir();
    let _ = std::process::Command::new("explorer.exe").arg(p).spawn();
}

/// 在应用内独立窗口打开任意白名单站点
async fn open_web_window(app: &AppHandle, url_str: String) -> Result<(), String> {
    let url = url_str.parse().map_err(|_| "URL 无效".to_string())?;

    if let Some(w) = app.get_webview_window("site") {
        let _ = w.set_title("PhuketBuddy · 站点");
        let _ = w.navigate(url);
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }

    let w = tauri::WebviewWindowBuilder::new(app, "site", tauri::WebviewUrl::External(url))
        .title("PhuketBuddy · 站点")
        .data_directory(secure::webview_data_dir())
        .inner_size(1040.0, 780.0)
        .min_inner_size(720.0, 520.0)
        .build()
        .map_err(|e| e.to_string())?;
    let _ = w.center();
    let _ = w.set_focus();
    Ok(())
}

/// 打开 PhuketAI 站点（应用内窗口）
#[tauri::command]
async fn open_site_window(app: AppHandle, path: String) -> Result<(), String> {
    let clean = path.trim();
    let suffix = if clean.is_empty() {
        String::new()
    } else if clean.starts_with('/') {
        clean.to_string()
    } else {
        format!("/{clean}")
    };
    open_web_window(&app, format!("https://ai.pukitcore.com{suffix}")).await
}

/// 打开白名单内的外部站点（DeepSeek 平台等）
#[tauri::command]
async fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    const ALLOW: [&str; 2] = ["https://ai.pukitcore.com", "https://platform.deepseek.com"];
    if !ALLOW.iter().any(|p| url.starts_with(p)) {
        return Err("不允许打开的地址".into());
    }
    open_web_window(&app, url).await
}

/* ------------------------- DeepSeek ------------------------- */


fn ds_key_tail(key: &str) -> String {
    let k = key.trim();
    if k.len() <= 8 {
        return "****".into();
    }
    format!("{}****{}", &k[..4], &k[k.len() - 4..])
}

fn ds_state() -> Value {
    let key = secure::load_ds_key();
    let tail = key.as_deref().map(ds_key_tail).unwrap_or_default();
    json!({
        "configured": key.is_some(),
        "tail": tail,
        "data": secure::load_ds_cache().unwrap_or(Value::Null),
    })
}

#[tauri::command]
async fn ds_status() -> Result<Value, String> {
    Ok(ds_state())
}

#[tauri::command]
async fn ds_set_key(app: AppHandle, key: String) -> Result<Value, String> {
    let state = app.state::<AppState>();
    let k = key.trim().to_string();
    if k.is_empty() {
        return Err("请填写 DeepSeek API Key".into());
    }
    // 先用一次真实请求校验，Key 无效就不保存
    let bal = api::deepseek_balance(&state.api.http, &k).await?;
    if !secure::save_ds_key(&k) {
        return Err("Key 已验证，但保存失败（数据目录不可写）".into());
    }
    secure::save_ds_cache(&bal);
    let _ = app.emit("pb://deepseek", bal.clone());
    Ok(json!({
        "configured": true,
        "tail": ds_key_tail(&k),
        "data": bal,
    }))
}

#[tauri::command]
fn ds_clear_key(app: AppHandle) -> Result<(), String> {
    secure::clear_ds_key();
    let _ = app.emit("pb://deepseek", Value::Null);
    Ok(())
}

#[tauri::command]
async fn ds_refresh(app: AppHandle) -> Result<Value, String> {
    let state = app.state::<AppState>();
    let key = secure::load_ds_key().ok_or("还没有绑定 DeepSeek API Key")?;
    let bal = api::deepseek_balance(&state.api.http, &key).await?;
    secure::save_ds_cache(&bal);
    let _ = app.emit("pb://deepseek", bal.clone());
    Ok(bal)
}

/* ------------------------- 气泡右键菜单 ------------------------- */

fn set_bubble_source(app: &AppHandle, src: &str) {
    let state = app.state::<AppState>();
    let mut s = state.settings.lock().map(|g| g.clone()).unwrap_or_default();
    s.bubble_source = src.to_string();
    secure::save_settings(&s);
    if let Ok(mut g) = state.settings.lock() {
        *g = s;
    }
    let _ = app.emit("pb://source", src);
    secure::log_line(&format!("bubble source -> {src}"));
}

fn popup_source_menu(
    app: &AppHandle,
    window: &WebviewWindow,
    current: &str,
    ds_bound: bool,
) -> Result<(), String> {
    use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};

    let phuket = CheckMenuItem::with_id(
        app, "src:phuket", "显示 PhuketAI 余额", true, current == "phuket", None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let ds_label = if ds_bound {
        "显示 DeepSeek 余额"
    } else {
        "显示 DeepSeek 余额（未绑定）"
    };
    let deepseek = CheckMenuItem::with_id(
        app, "src:deepseek", ds_label, ds_bound, current == "deepseek", None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let sep = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let open = MenuItem::with_id(app, "m:open", "打开面板", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let refresh = MenuItem::with_id(app, "m:refresh", "立即刷新", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(app, "m:quit", "退出 PhuketBuddy", true, None::<&str>)
        .map_err(|e| e.to_string())?;

    let menu = Menu::with_items(app, &[&phuket, &deepseek, &sep, &open, &refresh, &quit])
        .map_err(|e| e.to_string())?;
    window.popup_menu(&menu).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn show_bubble_menu(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    let (current, ds_bound) = {
        let state = app.state::<AppState>();
        let cur = state
            .settings
            .lock()
            .map(|s| s.bubble_source.clone())
            .unwrap_or_else(|_| "phuket".to_string());
        (cur, secure::load_ds_key().is_some())
    };
    popup_source_menu(&app, &window, &current, ds_bound)
}

/* ------------------------- 启动 ------------------------- */

fn show_panel(app: &AppHandle) {
    if let (Some(panel), Some(bubble)) = (
        app.get_webview_window("panel"),
        app.get_webview_window("bubble"),
    ) {
        win::place_panel(&panel, &bubble);
        let _ = panel.show();
        let _ = panel.set_focus();
    }
}

/// 显示 / 隐藏气泡（后台常驻的核心：隐藏后进程继续在托盘里跑）
fn toggle_bubble(app: &AppHandle) -> bool {
    if let Some(w) = app.get_webview_window("bubble") {
        let visible = w.is_visible().unwrap_or(false);
        if visible {
            let _ = w.hide();
            // 气泡藏起来时，面板也一起收起
            if let Some(p) = app.get_webview_window("panel") {
                let _ = p.hide();
            }
            false
        } else {
            let _ = w.show();
            let _ = w.set_focus();
            true
        }
    } else {
        false
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏气泡", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "立即刷新", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 PhuketBuddy", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &refresh, &settings, &quit])?;

    // 托盘必须有图标，否则在通知区域是空白/不可见
    let icon = app
        .default_window_icon()
        .cloned()
        .or_else(|| tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png")).ok());
    match &icon {
        Some(img) => secure::log_line(&format!("tray icon: {}x{}", img.width(), img.height())),
        None => secure::log_line("tray icon: NONE (托盘会不可见)"),
    }

    let mut builder = TrayIconBuilder::with_id("main")
        .icon(icon.unwrap_or_else(|| {
            tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))
                .expect("embedded tray icon")
        }))
        .menu(&menu)
        .tooltip("PhuketBuddy · 后台常驻中 · 单击显示/隐藏气泡")
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();
            match event {
                tauri::tray::TrayIconEvent::Click {
                    button: tauri::tray::MouseButton::Left,
                    button_state: tauri::tray::MouseButtonState::Up,
                    ..
                } => {
                    toggle_bubble(app);
                }
                tauri::tray::TrayIconEvent::DoubleClick {
                    button: tauri::tray::MouseButton::Left,
                    ..
                } => {
                    // 双击托盘：直接展开面板
                    dock_show(app);
                    if let Some(w) = app.get_webview_window("bubble") {
                        let _ = w.show();
                    }
                    if let Some(p) = app.get_webview_window("panel") {
                        if !p.is_visible().unwrap_or(false) {
                            let _ = toggle_panel(app.clone());
                        } else {
                            let _ = p.set_focus();
                        }
                    }
                }
                _ => {}
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => {
                toggle_bubble(app);
            }
            "refresh" => {
                let _ = app.emit("pb://refresh-request", ());
            }
            "settings" => {
                let _ = app.emit("pb://open-settings", ());
                show_panel(app);
            }
            "quit" => app.exit(0),
            _ => {}
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

pub fn run() {
    // 崩溃可追溯：panic 也会写进日志
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".to_string());
        secure::log_line(&format!("!!! PANIC at {loc}: {info}"));
    }));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            secure::log_line("second instance -> show panel");
            dock_show(app);
            if let Some(w) = app.get_webview_window("bubble") {
                let _ = w.show();
            }
            if let Some(p) = app.get_webview_window("panel") {
                if !p.is_visible().unwrap_or(false) {
                    let _ = toggle_panel(app.clone());
                } else {
                    let _ = p.set_focus();
                }
            }
        }))
        .setup(|app| {
            let session = secure::load_session();
            let settings = secure::load_settings();
            let api = Api::new();

            app.manage(AppState {
                api,
                session: Mutex::new(session),
                settings: Mutex::new(settings),
                last_blur: Mutex::new(None),
                panel_shown_at: Mutex::new(None),
                dock: Mutex::new(None),
                dock_hidden: Mutex::new(false),
                hovered: Mutex::new(false),
                pinned: Mutex::new(false),
            });

            // 气泡右键菜单（动态创建）的事件统一在这里处理
            app.on_menu_event(|app, event| match event.id().as_ref() {
                "src:phuket" => set_bubble_source(app, "phuket"),
                "src:deepseek" => set_bubble_source(app, "deepseek"),
                "m:open" => {
                    let _ = toggle_panel(app.clone());
                }
                "m:refresh" => {
                    let _ = app.emit("pb://refresh-request", ());
                }
                "m:quit" => app.exit(0),
                _ => {}
            });

            // 窗口在代码里创建：数据目录走 secure::webview_data_dir() 的可写回退
            secure::log_line(&format!(
                "start v{} data={:?} webview={:?}",
                app.package_info().version,
                secure::data_dir(),
                secure::webview_data_dir()
            ));

            // 诊断：PHUKETBUDDY_TEST_AUTOSTART=1 时自检注册表读写
            if std::env::var("PHUKETBUDDY_TEST_SAVE").ok().as_deref() == Some("1") {
                let mut s = secure::load_settings();
                s.theme = "light".to_string();
                let auto = secure::set_autostart(true);
                secure::save_settings(&s);
                secure::log_line(&format!(
                    "save self-test: autostart={auto:?}, settings written (theme=light)"
                ));
            }
            if std::env::var("PHUKETBUDDY_TEST_AUTOSTART").ok().as_deref() == Some("1") {
                match secure::set_autostart(true) {
                    Ok(m) => secure::log_line(&format!("autostart self-test: OK via {m}")),
                    Err(e) => secure::log_line(&format!("autostart self-test: FAILED: {e}")),
                }
            }
            let bubble_build = tauri::WebviewWindowBuilder::new(
                app,
                "bubble",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("PhuketBuddy")
            .data_directory(secure::webview_data_dir())
            .inner_size(82.0, 82.0)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .shadow(false)
            .build();
            match &bubble_build {
                Ok(_) => eprintln!("[pb] bubble window OK"),
                Err(e) => eprintln!("[pb] bubble window FAILED: {e} | {e:?}"),
            }

            let panel_build = tauri::WebviewWindowBuilder::new(
                app,
                "panel",
                tauri::WebviewUrl::App("panel.html".into()),
            )
            .title("PhuketBuddy")
            .data_directory(secure::webview_data_dir())
            .inner_size(348.0, 520.0)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .shadow(false)
            .visible(false)
            .build();
            match &panel_build {
                Ok(_) => eprintln!("[pb] panel window OK"),
                Err(e) => eprintln!("[pb] panel window FAILED: {e} | {e:?}"),
            }

            // 气泡初始位置（启动即贴边）
            if let Some(bubble) = app.get_webview_window("bubble") {
                let size = bubble.outer_size().unwrap_or(PhysicalSize::new(82, 82));
                let pos = win::load_pos().unwrap_or_else(|| {
                    let p = win::default_pos(size);
                    win::SavedPos { x: p.x, y: p.y }
                });
                let _ = bubble.set_position(tauri::PhysicalPosition::new(pos.x, pos.y));
                win::snap(&bubble);
                refresh_dock(app.handle());
                // 2.5 秒后自动半隐藏（鼠标移上去会滑出）
                let h = app.handle().clone();
                schedule_dock_hide(h, 2500);
                // 后台常驻模式：启动时只留托盘，不弹气泡
                let hidden = app
                    .state::<AppState>()
                    .settings
                    .lock()
                    .map(|s| s.start_hidden)
                    .unwrap_or(false);
                if hidden {
                    let _ = bubble.hide();
                    secure::log_line("start hidden (tray only)");
                }
            }

            // 气泡窗口事件：拖动时让面板跟随
            if let Some(bubble_win) = app.get_webview_window("bubble") {
                let handle = app.handle().clone();
                bubble_win.on_window_event(move |event| {
                    if let tauri::WindowEvent::Moved(_) = event {
                        if let (Some(p), Some(b)) = (
                            handle.get_webview_window("panel"),
                            handle.get_webview_window("bubble"),
                        ) {
                            if p.is_visible().unwrap_or(false) {
                                win::place_panel(&p, &b);
                            }
                        }
                    }
                });
            }

            // 面板失焦隐藏
            if let Some(panel) = app.get_webview_window("panel") {
                let handle = app.handle().clone();
                panel.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let st = handle.state::<AppState>();
                        let in_grace = st
                            .panel_shown_at
                            .lock()
                            .ok()
                            .and_then(|g| *g)
                            .map(|t| t.elapsed() < Duration::from_millis(1200))
                            .unwrap_or(false);
                        if in_grace {
                            secure::log_line("panel blur ignored (grace)");
                            return;
                        }
                        // 焦点回到自己（例如点了气泡）不隐藏
                        if win::foreground_is_ours() {
                            secure::log_line("panel blur ignored (ours)");
                            return;
                        }
                        // 只有「用户真的点到了别处」（失焦瞬间鼠标按着）才收起，
                        // 避免其它程序偷偷抢焦点导致面板莫名消失
                        if !win::left_button_down() && !win::foreground_is_desktop() {
                            secure::log_line(&format!(
                                "panel blur ignored (no click) fg={}",
                                win::foreground_info()
                            ));
                            return;
                        }
                        if let Some(w) = handle.get_webview_window("panel") {
                            if w.is_visible().unwrap_or(false) {
                                let fg = win::foreground_info();
                                secure::log_line(&format!("panel blur -> hide, fg={fg}"));
                                if let Ok(mut g) = st.last_blur.lock() {
                                    *g = Some(Instant::now());
                                }
                                let _ = w.hide();
                                if let Ok(mut g) = handle.state::<AppState>().pinned.lock() {
                                    *g = false;
                                }
                                schedule_dock_hide(handle.clone(), 350);
                            }
                        }
                    }
                });
            }

            if let Err(e) = build_tray(app.handle()) {
                secure::log_line(&format!("tray build failed: {e}"));
            } else {
                secure::log_line("tray ready");
            }

            // 后台定时刷新 + 启动时首次拉取
            if std::env::var("PHUKETBUDDY_OPEN_PANEL").ok().as_deref() == Some("1") {
                let h = app.handle().clone();
                let to_settings =
                    std::env::var("PHUKETBUDDY_OPEN_SETTINGS").ok().as_deref() == Some("1");
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1200)).await;
                    let _ = toggle_panel(h.clone());
                    if to_settings {
                        tokio::time::sleep(Duration::from_millis(700)).await;
                        let _ = h.emit("pb://open-settings", ());
                    }
                });
            }
            if std::env::var("PHUKETBUDDY_BACKGROUND").ok().as_deref() == Some("1") {
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(2000)).await;
                    let _ = enter_background(h);
                });
            }
            if std::env::var("PHUKETBUDDY_OPEN_SITE").ok().as_deref() == Some("1") {
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(900)).await;
                    match open_site_window(h, "/keys".to_string()).await {
                        Ok(_) => secure::log_line("auto open site: ok"),
                        Err(e) => secure::log_line(&format!("auto open site failed: {e}")),
                    }
                });
            }
            if demo_mode() {
                secure::log_line("demo mode enabled");
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(700)).await;
                    let _ = h.emit("pb://dashboard", demo_dashboard());
                });
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(600)).await;
                loop {
                    let (logged, mins) = {
                        let st = handle.state::<AppState>();
                        let logged = st.session.lock().map(|g| g.is_some()).unwrap_or(false);
                        let mins = st.settings.lock().map(|s| s.refresh_minutes).unwrap_or(5);
                        (logged, mins.max(1))
                    };
                    if logged {
                        let st = handle.state::<AppState>();
                        if let Ok(d) = load_dashboard(&st).await {
                            secure::save_cache(&d);
                            let _ = handle.emit("pb://dashboard", d);
                        }
                    }
                    // DeepSeek 余额跟着一起刷新（绑定了才拉）
                    if let Some(key) = secure::load_ds_key() {
                        let st = handle.state::<AppState>();
                        if let Ok(bal) = api::deepseek_balance(&st.api.http, &key).await {
                            secure::save_ds_cache(&bal);
                            let _ = handle.emit("pb://deepseek", bal);
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(mins as u64 * 60)).await;
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            boot, login, logout, dashboard, save_settings, bubble_down,
            toggle_panel, hide_panel, panel_resize, quit_app, data_path, open_data_dir,
            open_site_window, open_external, ds_status, ds_set_key, ds_clear_key, ds_refresh,
            bubble_hover, show_bubble_menu, enter_background
        ])
        .build(tauri::generate_context!())
        .expect("error while building PhuketBuddy");

    // 心跳日志：每 5 分钟一条，用来区分「崩溃」和「被结束」
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(300));
        secure::log_line("heartbeat");
    });

    app.run(|_app, event| {
        if let tauri::RunEvent::Exit = event {
            secure::log_line("exit event (normal shutdown)");
        }
    });
}

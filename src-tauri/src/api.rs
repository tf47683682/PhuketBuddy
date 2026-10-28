use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_BASE: &str = "https://ai.pukitcore.com";
pub const API_PREFIX: &str = "/api/v1";
pub const DEEPSEEK_BASE: &str = "https://api.deepseek.com";

/// DeepSeek 官方余额接口：GET /user/balance
pub async fn deepseek_balance(http: &reqwest::Client, key: &str) -> Result<Value, String> {
    let resp = http
        .get(format!("{DEEPSEEK_BASE}/user/balance"))
        .bearer_auth(key)
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("连接 DeepSeek 失败：{e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err("API Key 无效或无权限".into());
    }
    if !status.is_success() {
        let brief: String = text.chars().take(140).collect();
        return Err(format!("DeepSeek 返回 HTTP {}：{}", status.as_u16(), brief));
    }

    let v: Value = serde_json::from_str(&text).map_err(|_| "返回内容不是 JSON".to_string())?;
    let infos = v.get("balance_infos").and_then(|x| x.as_array()).cloned().unwrap_or_default();

    // 优先人民币，没有就取第一条
    let picked = infos
        .iter()
        .find(|i| i.get("currency").and_then(|c| c.as_str()) == Some("CNY"))
        .or_else(|| infos.first())
        .cloned()
        .unwrap_or(Value::Null);

    let num = |k: &str| -> Option<f64> {
        picked
            .get(k)
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| picked.get(k).and_then(|x| x.as_f64()))
    };

    Ok(json!({
        "available": v.get("is_available").and_then(|b| b.as_bool()).unwrap_or(false),
        "currency": picked.get("currency").cloned().unwrap_or(Value::Null),
        "total": num("total_balance"),
        "granted": num("granted_balance"),
        "toppedUp": num("topped_up_balance"),
        "raw": v,
        "updatedAt": now_secs(),
    }))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Session {
    pub access: String,
    pub refresh: String,
    pub expires_at: i64,
    #[serde(default)]
    pub user: Value,
}

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    Transport(String),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "登录状态已失效"),
            ApiError::Transport(e) => write!(f, "网络连接失败：{e}"),
            ApiError::Other(e) => write!(f, "{e}"),
        }
    }
}

pub struct Api {
    pub http: reqwest::Client,
    pub base: String,
}

impl Api {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("PhuketBuddy/0.1 (Windows; +https://ai.pukitcore.com)")
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http, base: DEFAULT_BASE.to_string() }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}{}", self.base, API_PREFIX, path)
    }

    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> Result<Value, ApiError> {
        let mut req = self.http.request(method, self.url(path));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| ApiError::Transport(friendly(&e)))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| ApiError::Transport(friendly(&e)))?;
        let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));

        if status.as_u16() == 401 {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let msg = parsed
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            return Err(ApiError::Other(msg));
        }
        Ok(parsed)
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<(Session, Value), ApiError> {
        let body = json!({ "email": email, "password": password });
        let resp = self
            .request(reqwest::Method::POST, "/auth/login", Some(body), None)
            .await?;
        let data = unwrap_data(&resp);
        let access = text_of(data, &["access_token", "accessToken", "token"])
            .ok_or_else(|| ApiError::Other("登录成功但未返回 access_token".into()))?;
        let refresh = text_of(data, &["refresh_token", "refreshToken"]).unwrap_or_default();
        let expires_in = num_of(data, &["expires_in", "expiresIn"]).unwrap_or(3600.0) as i64;
        let user = data.get("user").cloned().unwrap_or(Value::Null);
        Ok((
            Session {
                access,
                refresh,
                expires_at: now_secs() + expires_in.max(60),
                user,
            },
            user_from(&data),
        ))
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<Session, ApiError> {
        let body = json!({ "refresh_token": refresh_token });
        let resp = self
            .request(reqwest::Method::POST, "/auth/refresh", Some(body), None)
            .await?;
        let data = unwrap_data(&resp);
        let access = text_of(data, &["access_token", "accessToken", "token"])
            .ok_or_else(|| ApiError::Other("刷新令牌失败".into()))?;
        let refresh = text_of(data, &["refresh_token", "refreshToken"])
            .unwrap_or_else(|| refresh_token.to_string());
        let expires_in = num_of(data, &["expires_in", "expiresIn"]).unwrap_or(3600.0) as i64;
        Ok(Session {
            access,
            refresh,
            expires_at: now_secs() + expires_in.max(60),
            user: data.get("user").cloned().unwrap_or(Value::Null),
        })
    }
}

fn friendly(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "请求超时".into()
    } else if e.is_connect() {
        "无法连接服务器".into()
    } else {
        e.to_string()
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/* ---------- 容错解析工具 ---------- */

pub fn unwrap_data(v: &Value) -> &Value {
    if let Some(d) = v.get("data") {
        if !d.is_null() {
            return d;
        }
    }
    v
}

pub fn num_of(v: &Value, names: &[&str]) -> Option<f64> {
    for n in names {
        match v.get(*n) {
            Some(Value::Number(x)) => return x.as_f64(),
            Some(Value::String(s)) => {
                if let Ok(x) = s.trim().parse::<f64>() {
                    return Some(x);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn text_of(v: &Value, names: &[&str]) -> Option<String> {
    for n in names {
        match v.get(*n) {
            Some(Value::String(s)) if !s.is_empty() => return Some(s.clone()),
            Some(Value::Number(x)) => return Some(x.to_string()),
            Some(Value::Bool(b)) => return Some(b.to_string()),
            _ => {}
        }
    }
    None
}

pub fn user_from(v: &Value) -> Value {
    let u = match v.get("user") {
        Some(u) if !u.is_null() => u.clone(),
        _ => v.clone(),
    };
    json!({
        "id": u.get("id").cloned().unwrap_or(Value::Null),
        "email": text_of(&u, &["email", "username", "name"]).unwrap_or_default(),
        "nickname": text_of(&u, &["display_name", "nickname", "username"]).unwrap_or_default(),
        "balance": num_of(&u, &["balance", "credits", "quota", "remain_quota"]),
        "role": text_of(&u, &["role", "group"]).unwrap_or_default(),
        "raw": u,
    })
}

/* ---------- 日期（Asia/Shanghai） ---------- */

pub fn ymd_offset(days: i64) -> String {
    let secs = now_secs() + 8 * 3600 + days * 86400;
    let z = secs.div_euclid(86400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", yy, m, d)
}

/* ---------- 归一化 ---------- */

pub fn normalize_keys(v: &Value) -> Vec<Value> {
    let d = unwrap_data(v);
    let arr: Vec<Value> = if let Some(a) = d.as_array() {
        a.clone()
    } else {
        let mut found = Vec::new();
        for key in ["items", "keys", "list", "records"] {
            if let Some(a) = d.get(key).and_then(|x| x.as_array()) {
                found = a.clone();
                break;
            }
        }
        found
    };

    arr.into_iter()
        .map(|k| {
            let raw_key = text_of(&k, &["key", "full_key", "secret"]).unwrap_or_default();
            let status_val = k.get("status").cloned().unwrap_or(Value::Null);
            let enabled = match &status_val {
                Value::Number(n) => n.as_i64().unwrap_or(0) > 0,
                Value::String(s) => {
                    let s = s.to_lowercase();
                    s == "active" || s == "enabled" || s == "1" || s == "valid"
                }
                Value::Bool(b) => *b,
                _ => true,
            };
            json!({
                "id": k.get("id").cloned().unwrap_or(Value::Null),
                "name": text_of(&k, &["name", "title"]).unwrap_or_else(|| "未命名".into()),
                "key": raw_key,
                "masked": mask_key(&raw_key),
                "quota": num_of(&k, &["quota", "remain_quota", "balance", "credits"]),
                "enabled": enabled,
                "statusText": text_of(&k, &["status"]).unwrap_or_default(),
                "expiresAt": text_of(&k, &["expires_at", "expired_at", "expire_at"]).unwrap_or_default(),
                "group": text_of(&k, &["group", "group_name", "groupName"]).unwrap_or_default(),
            })
        })
        .collect()
}

pub fn mask_key(k: &str) -> String {
    if k.is_empty() {
        return "—".into();
    }
    if k.len() <= 12 {
        return format!("{}****", &k[..k.len().min(4)]);
    }
    format!("{}****{}", &k[..6], &k[k.len() - 4..])
}

pub fn normalize_trend(v: &Value) -> Vec<Value> {
    let d = unwrap_data(v);
    let arr: Vec<Value> = if let Some(a) = d.as_array() {
        a.clone()
    } else if let Some(a) = d.get("trend").and_then(|x| x.as_array()) {
        a.clone()
    } else if let Some(a) = d.get("items").and_then(|x| x.as_array()) {
        a.clone()
    } else {
        Vec::new()
    };

    arr.into_iter()
        .map(|p| {
            json!({
                "date": text_of(&p, &["date", "day", "time", "created_at"]).unwrap_or_default(),
                "cost": num_of(&p, &["cost", "actual_cost", "total_cost", "credits", "amount"]).unwrap_or(0.0),
                "requests": num_of(&p, &["requests", "count", "request_count"]).unwrap_or(0.0),
            })
        })
        .collect()
}

pub fn normalize_stats(v: &Value) -> Value {
    let d = unwrap_data(v);
    json!({
        "todayCost": num_of(d, &["today_actual_cost", "today_cost", "today_credits", "today_amount"]),
        "todayRequests": num_of(d, &["today_requests", "today_request_count", "today_count"]),
        "totalCost": num_of(d, &["total_actual_cost", "total_cost", "total_credits", "total_amount"]),
        "totalRequests": num_of(d, &["total_requests", "total_request_count", "total_count"]),
        "keyCount": num_of(d, &["total_api_keys", "api_key_count", "keys"]),
        "balance": num_of(d, &["balance", "credits", "remain_quota"]),
        "byPlatform": d.get("by_platform").cloned().unwrap_or(Value::Null),
    })
}

/* ---------- 诊断：只返回字段结构，不含数值 ---------- */

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub fn shape_of(v: &Value) -> Value {
    match v {
        Value::Array(a) => match a.first() {
            Some(first) => json!([shape_of(first)]),
            None => json!([]),
        },
        Value::Object(m) => {
            let mut o = serde_json::Map::new();
            for (k, val) in m {
                o.insert(k.clone(), json!(type_name(val)));
            }
            Value::Object(o)
        }
        _ => json!(type_name(v)),
    }
}

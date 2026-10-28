# PhuketBuddy

PhuketAI（`ai.pukitcore.com`）的桌面气泡助手：**余额优先**常驻桌面，点开即可查看用量与 API Key。

- 体积：单文件 **3.9 MB**，不依赖 .NET/VC 运行时（仅需系统自带 WebView2）
- 形态：**82px 圆形气泡**，显示「余额 + 今日已消费」；拖到哪就贴到最近的屏幕边，**平时半隐藏在边缘，鼠标移上去自动滑出**
- 外观：**深色 / 亮色**两套主题，设置里切换，气泡与面板同步
- 常驻：**隐藏气泡 / 转入后台后进程继续跑在托盘**，定时刷新不停；可设开机自启 + 启动即隐藏（纯托盘常驻）
- 数据：余额、今日消耗/请求、累计消耗、近 7 日趋势、API Key 列表（可复制、可跳转管理）
- **DeepSeek**：绑定 API Key 后显示余额与充值/赠送构成；整块信息**可折叠**（点标题栏收起/展开）
- 存储：密码不落盘；令牌与 Key 都用 Windows DPAPI 加密，仅存本机

---

## 直接使用

双击 `PhuketBuddy.exe`（或 `src-tauri\target\release\phuketbuddy.exe`）。

分发包：
- `PhuketBuddy-v0.1.0-win64.zip`（1.88 MB）：exe + 说明 + 截图，解压即用
- `PhuketBuddy-src-v0.1.0.zip`（0.38 MB）：完整源码（不含编译产物），解压后可 `cargo build --release` 复现

首次使用：点击气泡 → 输入 PhuketAI 的邮箱和密码 → 登录后气泡即显示余额。

| 操作 | 效果 |
|---|---|
| 单击气泡 | 展开 / 收起面板（面板打开期间气泡常显） |
| **鼠标移到边缘气泡** | 半隐藏状态自动滑出；移开约 0.6 秒后重新半隐藏 |
| **右键气泡** | 切换气泡显示 **PhuketAI / DeepSeek** 余额（未绑定时该菜单项置灰）；另有 打开面板 / 立即刷新 / 退出 |
| **单击托盘图标** | 显示 / 隐藏气泡（隐藏后仍在后台跑） |
| **设置 → 转入后台** | 同时收起面板和气泡，只留托盘图标；定时刷新继续，点托盘可随时恢复 |
| 拖动气泡 | 移动；松手**自动吸附到最近的屏幕边**，随后半隐藏 |
| **右键托盘图标** | 显示/隐藏气泡、立即刷新、设置、**退出 PhuketBuddy** |
| 面板右上齿轮 | 外观（深色/亮色）、自动刷新间隔、开机自启、**启动时隐藏气泡**、低余额阈值、退出登录、打开数据目录、退出程序 |
| 面板里的 Key 行 | 点击复制完整 Key |
| **充值** | 余额右侧的「充值」按钮 → 应用内打开站点购买页 |
| **DeepSeek 余额** | 面板底部卡片；首次在「设置 → DeepSeek API Key」里绑定。<br/>「查看用量」会在应用内打开 `platform.deepseek.com/usage` |
| 「管理 API Key」「打开网站」 | 全部在应用内窗口打开对应站点页面 |

数据位置（正常情况下）：

```
%APPDATA%\PhuketBuddy\session.bin        # DPAPI 加密的令牌
%APPDATA%\PhuketBuddy\settings.json      # 设置
%APPDATA%\PhuketBuddy\cache.json         # 上次数据（用于秒开）
%APPDATA%\PhuketBuddy\raw-shape.json     # 接口字段结构（诊断用，不含数值/密钥）
%LOCALAPPDATA%\PhuketBuddy\webview\      # WebView2 缓存
```

> 若上述目录不可写（受限环境），会自动回退到 exe 同级的 `data\` 目录。

---

## 从源码构建

已在本机准备好 Rust 工具链（装在 `G:\Project\.rust`，未污染系统 PATH）。

```powershell
.\build.ps1
```

脚本内容等价于：

```powershell
$env:CARGO_HOME  = 'G:\Project\.rust\cargo'
$env:RUSTUP_HOME = 'G:\Project\.rust\rustup'
$env:PATH        = 'G:\Project\.rust\cargo\bin;' + $env:PATH
$env:TEMP        = 'G:\Project\.tmp'   # 规避本机链接器的临时目录限制
cd G:\Project\PhuketBuddy\src-tauri
cargo build --release
```

产物：`src-tauri\target\release\phuketbuddy.exe`

### 目录结构

```
PhuketBuddy/
├── ui/                     # 前端（纯 HTML/CSS/JS，无打包器，直接内嵌进 exe）
│   ├── index.html          # 气泡窗口
│   └── panel.html          # 面板窗口（登录 / 仪表盘 / 设置）
└── src-tauri/
    ├── tauri.conf.json     # 应用配置（窗口在 lib.rs 里代码创建）
    ├── capabilities/
    ├── icons/
    └── src/
        ├── main.rs         # 入口（设置 WebView2 参数）
        ├── lib.rs          # 状态机、命令、托盘、定时刷新
        ├── api.rs          # HTTP 客户端 + 容错解析
        ├── secure.rs       # DPAPI、数据目录回退、自启
        └── win.rs          # 工作区计算、吸边、面板定位
```

---

## 接口（已实测）

| 用途 | 接口 |
|---|---|
| 登录 | `POST /api/v1/auth/login` `{email,password}` |
| 续期 | `POST /api/v1/auth/refresh` `{refresh_token}` |
| 用户/余额 | `GET /api/v1/auth/me` |
| 概览 | `GET /api/v1/usage/dashboard/stats` |
| 趋势 | `GET /api/v1/usage/dashboard/trend?start_date&end_date&granularity=day` |
| Key 列表 | `GET /api/v1/keys` |

> 站点是自有实现（非 new-api），字段名以 `raw-shape.json` 为准做防御式解析；
> 当前版本对 `balance / today_actual_cost / today_requests` 等做了多候选字段匹配。

---

## 已知限制

1. **首次真实账号联调**：仪表盘字段映射需用真实账号确认一次（登录后会自动在 `raw-shape.json` 留下字段结构）。
2. 透明窗口依赖关闭 WebView2 GPU 合成（已在 `main.rs` 固定），以兼容部分显卡驱动。
3. 未做代码签名，首次运行 SmartScreen 可能提示「更多信息 → 仍要运行」。
4. 安装包（NSIS）尚未生成；当前是绿色版单文件。

---

## 常见问题

**DeepSeek 为什么只有余额、没有用量明细？**

DeepSeek 官方 API 只开放了 `GET /user/balance`（余额、充值/赠送构成、可用状态），**没有对外提供 token 用量查询接口**。

面板里的 DeepSeek 卡片只显示官方余额信息，点标题栏可以折叠。要看用量明细，点卡片上的「查看用量」在应用内打开 `platform.deepseek.com/usage`（需在那里登录一次）。

**打开「开机自启」提示写入注册表失败？**

老版本会因此把整次保存都中断（主题也存不下来）——**已修复**：现在自启失败只回一条警告，其余设置照常保存。

自启本身有三层处理：① 写 `HKCU\...\Run`；② 失败则改在「启动」文件夹放一个 `PhuketBuddy.cmd`；③ 都失败才报错（通常意味着当前进程运行在受限令牌下，比如从某些代理/沙箱环境启动）。

如果两条路都被拦：用资源管理器直接双击 exe 再试一次；仍不行就手动把 exe 快捷方式放进 `shell:startup` 目录。

**「管理 API Key / 打开网站」怎么打开的？**

本版**不再调用系统浏览器**，全部在**应用内窗口**里打开官网（避免浏览器权限/关联异常导致点了没反应）。

应用内窗口有独立的登录态：第一次打开会显示官网登录页，登录一次之后会记住（存在软件自己的 WebView2 数据目录里）。

> 如果你更想用系统浏览器，可以自行在浏览器里打开 `https://ai.pukitcore.com/keys`。

---

## 调试技巧

用演示数据启动（无需账号，界面显示内置示例数据）：

```powershell
$env:PHUKETBUDDY_DEMO = '1'
.\PhuketBuddy.exe
```

运行时日志：数据目录下的 `error.log`（自动 256KB 轮转）。
面板 → 设置 → 数据目录 → 「打开」可直接定位。


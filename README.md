# pulse-agent

一个用 Rust 编写的轻量级服务器监控上报 Agent，对接 **Pulse** 面板。

- 单文件二进制，Windows / Linux 双平台
- 仅 2 个第三方依赖：`rustls`（`ring` provider）+ `webpki-roots`
- 纯标准库实现 HTTP/1.1、JSON、指标采集（Linux 走 `/proc`，Windows 走 Win32 FFI）
- 支持一键安装为系统后台服务（Linux systemd / Windows SCM）

接口契约见仓库内的 [`PULSE-API.md`](./PULSE-API.md)。

---

## 目录

- [功能](#功能)
- [依赖与体积](#依赖与体积)
- [构建](#构建)
- [快速开始](#快速开始)
- [命令](#命令)
- [选项](#选项)
- [环境变量](#环境变量)
- [上报流程与间隔](#上报流程与间隔)
- [安装为后台服务](#安装为后台服务)
- [卸载与清理](#卸载与清理)
- [日志](#日志)
- [采集内容与平台差异](#采集内容与平台差异)
- [退出码](#退出码)
- [安全说明](#安全说明)
- [常见问题](#常见问题)
- [项目结构](#项目结构)
- [开发与测试](#开发与测试)

---

## 功能

| 能力 | 说明 |
| --- | --- |
| 指标采集 | CPU 使用率/型号/核心数、内存、Swap、磁盘、网络上下行、主机名、系统版本、内核、架构、运行时长 |
| 定时上报 | 按固定间隔或面板下发的 `report_interval` 上报 |
| 单次上报 | `once` 命令，用于调试 |
| 自检 | `check` 命令校验 API KEY 并读取面板建议 |
| 服务化 | `install` / `uninstall` / `start` / `stop` / `restart` / `status` |
| HTTPS | 内置 Mozilla 根证书；支持 `--insecure` 关闭证书校验 |
| 日志等级 | `error` / `warn` / `info` / `debug`，默认 `error` |

---

## 依赖与体积

| crate | 作用 | 选择原因 |
| --- | --- | --- |
| `rustls` 0.23（`ring`） | TLS 客户端 | 纯 Rust，`ring` 不需要 CMake/NASM，交叉编译和静态链接更省事 |
| `webpki-roots` 1.0 | 内置根证书 | 不依赖宿主机 `/etc/ssl` 是否完整 |

未引入 serde / clap / sysinfo 等库，HTTP、JSON、CLI 解析与指标采集均为本仓库实现。

`release` 配置为 `opt-level = "z"` + LTO + strip，Windows 下实测约 **1.4 MB**。

最低 Rust 版本：**1.74**。

---

## 构建

```bash
# Windows（MSVC 工具链）
cargo build --release
# 产物: target\release\pulse-agent.exe

# Linux（glibc）
cargo build --release
# 产物: target/release/pulse-agent

# Linux 静态二进制（推荐用于分发）
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# 产物: target/x86_64-unknown-linux-musl/release/pulse-agent
```

> `ring` 含 C/汇编代码，交叉编译到 musl 需要对应的 C 交叉编译器
> （如在 Windows 上交叉编译需安装 `x86_64-linux-musl-gcc`；
> 在 Linux 上安装 `musl-tools` 即可）。最省事的方式是**直接在目标平台构建**。

---

## 快速开始

```bash
# 1) 自检：验证 KEY 并查看面板返回的信息
pulse-agent check --url https://pulse.example.com --key pk_xxxxxxxx

# 2) 单次上报：采集一次并发送（用于调试）
pulse-agent once --url https://pulse.example.com --key pk_xxxxxxxx

# 3) 前台持续运行
pulse-agent run --url https://pulse.example.com --key pk_xxxxxxxx

# 4) 安装为后台服务并启动
sudo pulse-agent install --url https://pulse.example.com --key pk_xxxxxxxx   # Linux
#   以管理员身份运行:
pulse-agent.exe install --url https://pulse.example.com --key pk_xxxxxxxx    # Windows
```

也可以用环境变量代替参数：

```bash
export PULSE_URL=https://pulse.example.com
export PULSE_API_KEY=pk_xxxxxxxx
pulse-agent install
```

---

## 命令

| 命令 | 说明 |
| --- | --- |
| `run` | 前台循环采集并上报（**默认命令**） |
| `once` | 只采集并上报一次，打印结果后退出 |
| `check` | 请求 `GET /api/v1/server`，校验 KEY 并显示服务器信息 |
| `install` | 安装为系统服务并启动 |
| `uninstall` | 停止服务、删除服务定义并清理安装文件 |
| `start` / `stop` / `restart` | 控制已安装的服务 |
| `status` | 查看服务状态 |
| `help` | 显示帮助（等价 `-h` / `--help`） |
| `version` | 显示版本（等价 `-V` / `--version`） |

---

## 选项

命令行参数优先于环境变量。

| 选项 | 说明 | 默认 |
| --- | --- | --- |
| `-u, --url <URL>` | 面板地址，如 `https://pulse.example.com`；可带或不带 `/api/v1` | — |
| `-k, --key <KEY>` | 服务器 API KEY（`pk_` 开头） | — |
| `-i, --interval <秒>` | 固定上报间隔；设置后忽略面板下发的建议值 | 跟随面板，兜底 60 |
| `--timeout <秒>` | HTTP 连接/读写超时 | `10` |
| `--insecure` | 跳过 TLS 证书校验（自签名证书时使用） | 关闭 |
| `--service-name <名>` | 服务名；Linux 下即 systemd unit 名 | `PulseAgent` |
| `--install-dir <目录>` | 安装目录（覆盖平台默认值） | 见下文 |
| `--log-file <文件>` | 追加日志到文件 | — |
| `--log-level <级别>` | `error` / `warn` / `info` / `debug` | `error` |
| `-v, --verbose` | 等价于 `--log-level debug` | — |
| `-h, --help` | 显示帮助 | — |
| `-V, --version` | 显示版本 | — |

> `--server`、`--server-url` 可作为 `--url` 的别名；`--api-key` 可作为 `--key` 的别名。

### 面板地址规范化

`--url https://host`、`https://host/`、`https://host/api/v1` 均可。
Agent 会自动补全为 `.../api/v1`，并请求：

- `POST {base}/report` — 上报
- `GET  {base}/server` — 自检

---

## 环境变量

| 变量 | 对应选项 |
| --- | --- |
| `PULSE_URL` | `--url` |
| `PULSE_API_KEY` / `PULSE_KEY` | `--key` |
| `PULSE_INTERVAL` | `--interval` |
| `PULSE_TIMEOUT` | `--timeout` |
| `PULSE_INSECURE=1` | `--insecure` |
| `PULSE_SERVICE_NAME` | `--service-name` |
| `PULSE_INSTALL_DIR` | `--install-dir` |
| `PULSE_LOG_FILE` | `--log-file` |
| `PULSE_LOG_LEVEL` | `--log-level` |
| `PULSE_VERBOSE=1` | 等价 `--log-level debug` |

---

## 上报流程与间隔

1. 启动时（未指定固定间隔时）调用 `GET /api/v1/server`，读取 `report_interval`。
2. 采集指标：CPU 使用率与网速通过 **间隔 1 秒的两次采样求差** 得到。
3. `POST /api/v1/report`，请求头带 `Authorization: Bearer <KEY>`。
4. 每次成功上报后，若未指定 `--interval`，采用响应中的 `report_interval` 作为下个间隔。
5. 失败时记录错误，并按当前间隔重试（无面板校验，提前/延后上报均可）。

`server_time` 以 **Unix 时间戳（秒）** 形式上报，API 直接支持该格式，因此不做时区换算。

---

## 安装为后台服务

### Linux（systemd）

`install` 需要 root：

```bash
sudo ./pulse-agent install --url https://pulse.example.com --key pk_xxxxxxxx
```

行为：

1. 校验参数，复制程序到 `/usr/local/bin/pulse-agent`（可用 `--install-dir` 覆盖）；
2. 生成 `/etc/systemd/system/<name>.service`（文件权限 `600`，KEY 写在 `Environment=` 中）；
3. 执行 `systemctl daemon-reload`、`enable`、`restart`。

生成的 unit 关键内容：

```ini
[Service]
Type=simple
Environment="PULSE_URL=https://pulse.example.com/api/v1"
Environment="PULSE_API_KEY=pk_xxxxxxxx"
Environment="PULSE_LOG_LEVEL=error"
ExecStart=/usr/local/bin/pulse-agent run
Restart=always
RestartSec=10
NoNewPrivileges=true
ProtectSystem=full
ProtectHome=read-only
PrivateTmp=true
```

查看日志：`journalctl -u PulseAgent -f`

### Windows（SCM）

`install` 需要**管理员权限**：

```powershell
.\pulse-agent.exe install --url https://pulse.example.com --key pk_xxxxxxxx
```

行为：

1. 复制程序到 `%ProgramFiles%\PulseAgent\pulse-agent.exe`；
2. 通过 SCM API 创建服务（服务名 `PulseAgent`，显示名 `Pulse Agent`，自动启动）；
3. 将配置写入服务注册表 `HKLM\SYSTEM\CurrentControlSet\Services\<服务名>\Environment`（`REG_MULTI_SZ`）；
4. 默认日志文件为 `<安装目录>\pulse-agent.log`；
5. 启动服务。

服务进程通过手写的 `StartServiceCtrlDispatcherW` 派发器接入 SCM；直接双击或命令行启动同一程序时会自动回落到前台模式。

```powershell
.\pulse-agent.exe status
.\pulse-agent.exe stop
.\pulse-agent.exe start
.\pulse-agent.exe restart
```

---

## 卸载与清理

```bash
sudo ./pulse-agent uninstall        # Linux
```

```powershell
.\pulse-agent.exe uninstall          # Windows（管理员）
```

- **Linux**：停止并禁用服务，删除 unit 文件，执行 `daemon-reload`，删除 `/usr/local/bin/pulse-agent`。
  仅当显式使用了 `--install-dir` 时，才尝试删除该（已空的）目录，绝不递归删除共享目录。
- **Windows**：停止并删除服务，递归删除安装目录及其中所有文件。
  - 安全护栏：拒绝删除盘符根、`Program Files`、`Windows`、`System32`、`Users`、`ProgramData` 等路径；
    且仅当目录中存在 `pulse-agent.exe` 时才执行删除。
  - 若当前运行的程序正位于该目录（文件被占用），会启动一个延迟助手，在进程退出后完成删除。

---

## 日志

- 同时输出到 **stderr**（Linux 下由 systemd 收集到 journal）与 `--log-file` 指定的文件。
- 等级从低到高：`error` → `warn` → `info` → `debug`，**默认 `error`**。
- `install` 会把当前等级持久化到服务定义（systemd `Environment=` / Windows 服务 `Environment`）。
- `request body` 不会被打日志；`debug` 级别只记录上报体的字节数。

示例：

```bash
pulse-agent run --url ... --key ... --log-level info
pulse-agent once --url ... --key ... -v          # 等价 debug
PULSE_LOG_LEVEL=info pulse-agent run
```

---

## 采集内容与平台差异

上报体为嵌套结构（字段说明见 `PULSE-API.md`）：

```json
{
  "server_time": 1794218991,
  "timezone": "Asia/Shanghai",
  "hostname": "web-01",
  "os": "Ubuntu 22.04.4 LTS",
  "kernel": "5.15.0-91-generic",
  "arch": "x86_64",
  "uptime": 864000,
  "cpu":    { "model": "Intel Xeon E5-2680 v4", "cores": 8, "usage": 23.5 },
  "memory": { "total": 8589934592, "used": 3221225472 },
  "swap":   { "total": 2147483648, "used": 0 },
  "disk":   { "total": 107374182400, "used": 53687091200 },
  "network":{ "up": 125000, "down": 890000 }
}
```

| 字段 | Linux 来源 | Windows 来源 |
| --- | --- | --- |
| `hostname` | `/proc/sys/kernel/hostname` | `GetComputerNameW` |
| `os` | `/etc/os-release` 的 `PRETTY_NAME` | `RtlGetVersion`（映射产品名 + build） |
| `kernel` | `/proc/sys/kernel/osrelease` | `RtlGetVersion` 的 `major.minor.build` |
| `arch` | 编译目标架构 | 编译目标架构 |
| `uptime` | `/proc/uptime` | `GetTickCount64` |
| `cpu.model` | `/proc/cpuinfo` | 注册表 `ProcessorNameString` |
| `cpu.cores` | `/proc/cpuinfo` 的 `processor` 行数 | `GetSystemInfo` |
| `cpu.usage` | `/proc/stat` 两次采样差值 | `GetSystemTimes` 两次采样差值 |
| `memory` | `/proc/meminfo`（`MemTotal - MemAvailable`） | `GlobalMemoryStatusEx` |
| `swap` | `SwapTotal / SwapFree` | 页面文件总量/已用（扣除物理内存部分） |
| `disk` | `/proc/mounts` + `statvfs`，汇总真实设备（按设备去重，排除伪文件系统） | 固定磁盘 `GetDriveTypeW` + `GetDiskFreeSpaceExW` 汇总 |
| `network` | `/proc/net/dev`（排除 `lo`）两次采样差值 | `GetIfTable`（排除回环）两次采样差值 |

说明：

- CPU 使用率与网速为 **1 秒窗口** 的瞬时值（`network.up` 为发送，`down` 为接收）。
- `cpu.usage` 四舍五入到 2 位小数。
- Linux 的 `timezone` 依次取 `TZ`、`/etc/timezone`、`/etc/localtime` 的 `zoneinfo/` 路径。
- Windows 的 `timezone` 为系统的本地化时区名（如“中国标准时间”）。由于 `server_time` 使用 Unix 时间戳，该字段仅用于展示，不影响解析。
- 所有字段均可缺省；采集失败时该字段不会上报（面板图表相应断开，而不是记为 0）。

---

## 退出码

| 退出码 | 含义 |
| --- | --- |
| `0` | 成功 |
| `1` | 运行期错误（网络失败、服务操作失败等） |
| `2` | 参数或配置错误（缺少 `--url`/`--key`、未知选项、非法间隔等） |

---

## 安全说明

- API KEY 只会写入：
  - Linux：systemd unit 的 `Environment=`（文件权限 `600`）；
  - Windows：服务注册表项 `Environment`（仅管理员可读）。
- 运行时不把 KEY 放在进程命令行里（服务模式），避免被 `ps`/任务管理器直接看到。
- 请在服务端妥善保存 KEY；泄露后立即在面板重置。
- `--insecure` 会**关闭证书校验**，仅建议用于自签名面板或内网测试环境。
- 交互式使用 `--key` 时，KEY 可能进入 shell 历史记录；更安全的方式是使用环境变量。

---

## 常见问题

**`401 invalid_api_key` / `missing_api_key`**
KEY 不正确或未提供。先用 `pulse-agent check` 验证。

**`422 empty_report`**
有效监控字段为空（极少见）。可用 `-v` 观察是否采集异常。

**TLS 握手失败 / 证书错误**
面板使用自签名证书时加 `--insecure`；或为其配置受信任证书。

**Windows `install` 报“错误码 5”**
需要以管理员身份打开 PowerShell 再执行。

**Windows 服务启动后无日志**
默认日志等级为 `error`，正常运行不会输出。用
`install --log-level info` 重新安装，或查看 `<安装目录>\pulse-agent.log`。

**Windows 服务无法启动**
确认安装时使用的 `--service-name` 与运行时一致（`install` 已自动写入 `PULSE_SERVICE_NAME`）。
也可检查 `install` 是否成功写入了 `Environment` 注册表值。

**Linux 下 `systemctl status` 显示 active，但面板没有数据**
用 `journalctl -u PulseAgent -f`，并临时把日志等级调为 `debug` 排查（`--log-level debug` 后重新 `install`）。

**`status` 提示“服务不存在”**
尚未安装，或服务名不一致；用 `install` 或对应的 `--service-name`。

---

## 项目结构

```
pulse-agent/
├── Cargo.toml
├── PULSE-API.md          # 面板接口契约
├── README.md
└── src/
    ├── main.rs           # 入口与命令分发
    ├── cli.rs            # 参数/环境变量解析、帮助文本
    ├── json.rs           # 极简 JSON 读写
    ├── http.rs           # HTTP/1.1 客户端（std + rustls）
    ├── agent.rs          # 上报循环、响应解释、信号处理
    ├── log.rs            # 分级日志
    ├── util.rs           # 时间格式化、子进程工具
    ├── metrics/
    │   ├── mod.rs        # Sample 结构与 JSON 序列化
    │   ├── linux.rs      # /proc + statvfs 采集
    │   └── windows.rs    # Win32 FFI 采集
    └── service/
        ├── mod.rs
        ├── linux.rs      # systemd
        └── windows.rs    # SCM 服务 + 服务派发器 FFI
```

---

## 开发与测试

```bash
cargo build --release
cargo test
```

单元测试覆盖：URL 解析、HTTP 响应解析（`Content-Length` / `chunked`）、JSON 解析与转义、
CLI 参数解析、上报体序列化、Windows 删除目录安全护栏。

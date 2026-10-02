# NyaRemoteControl 架构设计

| 项目 | 内容 |
|---|---|
| 状态 | v0.4：第一阶段（M0–M7）和大部分第二、三阶段功能已实现；实机验证情况见 §13。v0.2：协议兼容、多显卡与笔记本；v0.3：按实现修订 §2、§5、§6；v0.4：按 09-27 之后的实现全面修订（见下方"调整摘要"和 §14） |
| 日期 | 2026-10-02 |
| 范围 | 整体架构 + 第一阶段（Windows ↔ Windows 可用版）详细设计；之后新增的设计见 §14，剩余规划见 §12 |

### v0.4 调整摘要（与最初方案不同、以现在为准的地方）

| 最初方案 | 现在 | 原因 |
|---|---|---|
| 被控端一个 `nya-server.exe`（service / helper / standalone 子命令） | 本体 `nya-server-svc.exe` + 管理程序 `nya-server.exe`，通过控制管道通信（§1.3） | 管理界面开着时也能停服务、替换本体文件，为自动更新做准备 |
| 一个 workspace 内含所有 crate | 三个仓库 common / server / client；server 自身是 workspace（§2） | server、client 独立发版 |
| 不做多用户 | 同一被控端可连多个客户端：一人操作、其余观看，可接管或顶掉（§14.3） | 消费版 Windows 只有一个桌面会话，按"共享屏幕 + 明确接管"处理 |
| 第一阶段不做 HDR | HDR 桌面按 FP16 截屏，在显卡上转换为 SDR 再编码（§3.6）；HDR 原样传输仍未做 | 开 HDR 的被控端原来画面发白 |
| 远端分辨率跟随窗口放到第二阶段 | 已实现：虚拟显示器（VDD）跟随客户端窗口，另有隐私屏（§14.1） | |
| 静止细化：静止 150 ms 后补一帧低 QP 帧 | 静止 60 ms 后补发 4 帧（§3.2） | 实现时调整 |
| 音频抖动缓冲：固定 30 ms，水位过高丢样本 | 自适应 20–80 ms，时钟漂移用 ±0.5 % 微调播放速度消除（§5） | 原做法延迟会在 30–120 ms 之间来回跳 |
| 统计浮层只有中位数 | 中位数 + 最近 10 秒的 P99，外加声音缓冲状态（§8） | 卡顿体现在 P99 |
| 客户端界面：winit 窗口 + 浮层 | 主界面是 WebView2 中的 Svelte 页面，远程画面在单独窗口，会话工具条为 egui（§14.7） | egui 界面局促、样式陈旧；网页更容易做出 UU / 向日葵那样的外观 |
| 第二、三阶段的码率自适应、多显示器、文件传输、剪贴板图片 / 文件、麦克风、USB、手柄、AMF | 均已实现（§12） | |
| （没有） | 安装包、CI、GitHub Releases 发布、带回滚的自动更新（§14.8） | 被控端在远处，更新失败不能失联 |

---

## 0. 目标与非目标

### 目标
- 自研远程桌面：被控端装服务端，本机装客户端，**不依赖 RDP**，体验对标商用云电脑（天翼云电脑等）。
- 同时覆盖 **办公 / 写代码 / 看视频 / 玩游戏** 四类场景，通过"办公模式 / 游戏模式"兼顾清晰度与流畅度。
- 被控端硬件编码：**NVIDIA（NVENC）优先，Intel 核显（QSV）同时支持**，AMD（AMF）暂缓但接口预留。
- **支持多显卡与笔记本混合显卡**（Optimus、独显直连 / MUX 切换），显示器接在哪块 GPU 都能采集，编码 GPU 可自动或手动选择。
- **协议兼容**：新旧版本的客户端与服务端可以互相连接，新功能通过协商启用（见 §6.4）。
- 附着在**当前控制台会话**上工作，不创建新会话；**锁屏、登录界面、UAC 弹窗**均可操作。
- 客户端：先 Windows，后 Android，核心逻辑用 Rust 共享。
- 外设（后续阶段）：声音、麦克风、文件夹挂载、打印机、USB、手柄；**尽量复用已签名的现成驱动，不自写内核驱动**。

### 非目标
- **不做内网穿透 / 中转 / 账号体系**。网络由外部组网工具（Tailscale / EasyTier / ZeroTier 等）负责，本程序只连对方 IP:端口。
- 不做多会话（不给每个连接者创建独立桌面）。同一被控机可以有多个客户端同时连接，但同一时刻只有一个操作者，其余观看（§14.3）。
- 不支持 RDP 会话内运行（只服务控制台会话）。
- Android 客户端不做多画面窗口（手机一次只显示一块屏幕，改为面板里切换显示器，§14.9）。

### 目标环境
- 被控端：Windows 10 21H2+ / Windows 11，x64，NVIDIA（GTX 10 系及以上）或 Intel 核显（Gen9+，HEVC 4:4:4 需 Gen11+）。
- 客户端：Windows 10/11 x64（第一阶段）。
- 网络：局域网或组网虚拟网（MTU 可能低至 1280）。

---

## 1. 总体架构

```
┌──────────────────────────── 被控端 ────────────────────────────┐
│                                                                │
│  nya-server.exe   管理程序（界面 + 命令行，不链接 FFmpeg）     │
│            ▲  控制管道 \\.\pipe\NyaRemoteControl.control       │
│            ▼                                                   │
│  nya-server-svc.exe service（Session 0，SYSTEM，Windows 服务） │
│   ├─ QUIC 监听、认证、连接管理（网络只在这里），多客户端分发   │
│   ├─ 监控会话切换（登录/注销/锁屏/切换用户）                   │
│   ├─ 在活动控制台会话中拉起 / 重启 helper                      │
│   ├─ 发送 Ctrl+Alt+Del（SendSAS）、USB/IP 隧道                 │
│   └─ 检查更新、启动独立的更新程序                              │
│            ▲  命名管道（仅 SYSTEM 可访问）                     │
│            ▼                                                   │
│  nya-server-svc.exe helper（活动控制台会话，SYSTEM 令牌）      │
│   ├─ 画面：DXGI 桌面复制 → 颜色转换 → NVENC/QSV/AMF 编码       │
│   │        每个推流槽位（slot）一条管线；虚拟显示器 / 隐私屏   │
│   ├─ 光标：形状 + 位置单独上报                                 │
│   ├─ 声音：进程回采（排除自身）→ Opus；麦克风 → VB-Cable       │
│   ├─ 输入：SendInput（跟随输入桌面切换）、虚拟手柄（ViGEm）    │
│   └─ 剪贴板：文字、图片、文件（粘贴时才传）                    │
└────────────────────────────────────────────────────────────────┘
                         │  QUIC（UDP，TLS 1.3）
                         │  经由外部组网工具
┌──────────────────────── 客户端 ────────────────────────────────┐
│  nya-client.exe                                                │
│   ├─ 界面：主界面（WebView2）+ 每个会话 / 每路画面一个窗口     │
│   ├─ 网络：QUIC 连接、流分发；可同时连接多台被控端             │
│   ├─ 视频：D3D11VA 硬件解码 → Shader 转 RGB → 翻转交换链呈现   │
│   ├─ 光标：本地绘制（零延迟）                                  │
│   ├─ 声音：Opus → 自适应抖动缓冲 → WASAPI；麦克风采集          │
│   ├─ 输入：键鼠采集（扫描码）、XInput 手柄，发送               │
│   ├─ 文件 / 剪贴板、USB 设备（usbipd-win）                     │
│   └─ 统计面板：帧率 / 码率 / 各环节延迟（中位数与 P99）        │
└────────────────────────────────────────────────────────────────┘
```

### 1.1 为什么拆成 service + helper 两个进程
| 问题 | 解决 |
|---|---|
| 服务运行在 Session 0，**看不到用户桌面**，无法采集画面和注入输入 | helper 运行在活动控制台会话内 |
| 普通用户进程**无法访问 Winlogon 桌面**（锁屏、登录界面、UAC 安全桌面） | helper 使用 **SYSTEM 令牌**（复制服务令牌并改 SessionId）启动 |
| 用户注销 / 切换用户时，会话内进程会被销毁或换会话 | 网络连接放在 service 中，helper 重启时**连接不断**，客户端只会感到短暂卡顿 |
| Ctrl+Alt+Del 无法通过 SendInput 模拟 | service 调用 `SendSAS`（需开启策略 `SoftwareSASGeneration`） |

service 与 helper 是**同一个可执行文件（`nya-server-svc.exe`）的不同子命令**，便于部署。

### 1.2 开发模式
开发阶段提供 `nya-server-svc.exe standalone`：单进程、普通用户权限、直接监听网络、不涉及服务与会话切换。里程碑 M1–M3 全部在此模式下完成，M4 再接入 service/helper。开发模式也提供控制管道（`NyaRemoteControl.control.standalone`）。

### 1.3 管理程序与控制管道
- 管理程序 `nya-server.exe` 只依赖 `nya-server-core`，**不链接 FFmpeg 和采集代码**，所以它开着时也能停服务、替换 `nya-server-svc.exe` 和 DLL。（同一 crate 出两个 bin 的做法行不通：MSVC 会保留任何被引用目标文件里的 FFmpeg 导入，FFmpeg 的 GNU 风格导入库又不支持 /DELAYLOAD，只能拆 crate。）
- 控制管道 `\\.\pipe\NyaRemoteControl.control`：协议在 `server/core/proto/control.proto`，和网络协议一样只能新增字段（升级过程中两边可能版本不同）。SDDL 允许 SYSTEM 完全访问、管理员和交互用户读写；是否为管理员由服务端模拟客户端令牌判断，只有管理员能看配对码、改设置。
- 设置通过管道修改时立即生效（必要时重启 helper、重新绑定端口并更新防火墙规则）；服务没运行时管理程序直接读写配置文件。

---

## 2. 代码仓库结构

四个独立仓库放在同级目录，共享一个 `target/`；FFmpeg 等第三方文件放在不属于任何仓库的 `third_party/`。server、client、android 各自发版，发布时用 `COMMON_REF` 文件固定所用的 common 提交：

```
NyaRemoteControl/
├─ common/   仓库：公共 crate、Web 界面、脚本与本文档
├─ server/   仓库：被控端（Cargo workspace，见下）
├─ client/   仓库：nya-client（Windows 客户端）
├─ android/  仓库：Android 客户端（Kotlin 界面 + Rust 核心，§14.9）
├─ signing/  不属于任何仓库：Android 发版签名密钥（备份到密码管理器）
└─ third_party/   FFmpeg 8.1 LGPL 预编译包、ViGEmClient 源码、可选组件安装包
                  （common/scripts/fetch-*.ps1 下载，固定版本并校验 SHA-256）
```

common 仓库：

```
common/
├─ crates/
│  ├─ nya-proto/        协议：.proto、帧头、流/数据报类型、版本与功能协商、统计百分位；无平台依赖（Android 共用）
│  │  ├─ proto/history/vX.Y/   每个发布过的协议版本冻结的 .proto
│  │  └─ tests/compat/vX.Y/    对应版本编码的样例消息（兼容性测试）
│  ├─ nya-transport/    QUIC 封装、证书指纹、配对 HMAC、文件传输与剪贴板文件的收发记账（Android 共用）
│  ├─ nya-ffmpeg-sys/   FFmpeg 预生成绑定与链接
│  ├─ nya-media/        编码器 / 解码器、Opus（链接 FFmpeg）
│  ├─ nya-jitter/       音频自适应抖动缓冲，纯 Rust（Windows 客户端经 nya_media::jitter 使用，Android 共用）
│  ├─ nya-win/          Windows 平台层：D3D11、显卡拓扑、DXGI 复制、颜色转换、跨显卡拷贝、
│  │                    SendInput、WASAPI、桌面切换、CCD 显示配置、设备节点、剪贴板（含 OLE 虚拟文件）
│  ├─ nya-ui/           egui on D3D11（会话工具条、统计面板），带中文字体
│  └─ nya-webui/        wry/WebView2 宿主：nya:// 协议嵌入页面、JSON 调用与事件桥
├─ web/                 Vite + Svelte 5 + TS：client.html（客户端主界面）、manager.html（被控端管理界面）
├─ scripts/             fetch-*.ps1、release-lib.ps1（两边发版脚本共用）
└─ docs/design/         本文档、界面设计稿 ui-redesign.html
```

server 仓库是一个 workspace：

| 目录 | 包 | 产物 |
|---|---|---|
| `.` | `nya-server` | 本体 `nya-server-svc.exe`：service、helper、standalone、diag、vdd-test、apply-update |
| `core/` | `nya-server-core` | 两个程序共用：配置、配对数据、路径、日志、安装、可选组件、控制管道协议与客户端、更新程序 |
| `manager/` | `nya-server-manager` | 管理程序 `nya-server.exe`：界面（manager.html）+ 命令行；只依赖 core |

client 仓库：单个 crate `nya-client`，`src/app/` 下是窗口逻辑（launcher 主界面、conn 多会话、extra 额外画面窗口、update 自动更新）。

### 2.1 主要依赖

| 用途 | 依赖 | 说明 |
|---|---|---|
| Windows API | `windows`（微软官方 crate） | D3D11 / DXGI / WASAPI / 服务 / 安全 API |
| 异步运行时 | `tokio` | 仅用于网络与 IPC；媒体处理走独立线程 |
| QUIC | `quinn` + `rustls` | TLS 1.3，支持流与不可靠数据报 |
| 序列化 | `prost` + `protox` | Protobuf：天然支持加字段、忽略未知字段，保证兼容；`protox` 为纯 Rust 编译器，无需安装 protoc。视频帧头为手写二进制（带长度，可扩展） |
| 视频编解码 | 自带的 `nya-ffmpeg-sys`（FFmpeg 8.1 预生成绑定，链接共享库） | 编码：`*_nvenc`、`*_qsv`、`*_amf`、`libopenh264`；解码：`d3d11va` + 软件回退 |
| 音频编解码 | FFmpeg 内置的 libopus | 48 kHz 立体声，不另外引入库 |
| 窗口 | `winit` | 客户端窗口与事件循环；渲染直接用 D3D11 交换链 |
| 界面 | `wry`（WebView2）+ Svelte 5；`egui` | 客户端主界面和被控端管理界面是网页（构建时 npm 打包后嵌入 exe）；会话工具条和统计面板用 egui 画在远程窗口上 |
| 安装包 | NSIS | 被控端、客户端各一个安装包 + 便携 zip，GitHub Actions 按 tag 构建发布 |
| 日志 | `tracing` + `tracing-appender` | 文件日志、按天滚动 |
| 配置 | `toml` + `serde` | 仅用于本地配置文件，不用于网络协议 |
| 错误处理 | `anyhow`（bin）/ `thiserror`（lib） | |

FFmpeg 采用 BtbN 预编译的 LGPL 共享库，只需分发 `avcodec-62.dll`、`avutil-60.dll`、`swresample-6.dll`（OpenH264、libopus 已静态编入）。C 运行时静态链接（`+crt-static`），目标机无需安装 VC++ 运行库。

---

## 3. 视频管线（核心）

### 3.1 被控端

```
DXGI AcquireNextFrame ──► 桌面纹理（BGRA，GPU）
        │ 同时取得：脏矩形 / 移动矩形 / 光标信息
        ▼
颜色转换（D3D11 Compute Shader，自己实现）
        │   4:2:0 → NV12      4:4:4 → YUV444（平面）
        ▼
FFmpeg 硬件帧（AV_PIX_FMT_D3D11 / QSV 映射）── 全程不回 CPU
        ▼
NVENC / QSV 编码 ──► Annex-B 码流 ──► 帧头 + 数据 ──► IPC ──► service ──► QUIC
```

关键决策：

| 项目 | 决策 | 理由 |
|---|---|---|
| 采集 API | DXGI Desktop Duplication | 能拿到脏矩形/移动矩形/光标，可采集安全桌面；Sunshine 同方案 |
| 颜色转换 | **自己写 Compute Shader** | 色彩矩阵（BT.709）与范围完全可控，NVENC/QSV 行为一致；4:4:4 也走同一套 |
| 编码器接口 | 通过 FFmpeg 统一调用 | 一套代码覆盖 NVENC / QSV，将来加 AMF 只是换编码器名 |
| 零拷贝 | 采集纹理 → `CopySubresourceRegion` 进 FFmpeg 帧池纹理 | 唯一一次 GPU 内拷贝，无 CPU 往返 |
| 无变化时 | **不发帧** | 静止桌面带宽趋近 0 |
| 编码参数 | 无 B 帧；CBR/低 VBV；超长 GOP；**丢包时由客户端请求关键帧**；NVENC 低延迟预设（P1–P4 + `tune=ull`） | 低延迟 |

采集 GPU 与编码 GPU 可以不同（例如笔记本内屏挂在核显、用独显 NVENC 编码），见 §3.5。

### 3.2 两种模式

| | 办公模式（默认） | 游戏模式 |
|---|---|---|
| 编码 | HEVC 4:4:4（设备不支持时回退 4:2:0 并提高码率） | HEVC 或 H.264，4:2:0 |
| 帧率 | 最高 60，按变化驱动 | 跟随客户端显示器刷新率（上限 `max_fps`，默认 144），稳定输出 |
| 静止细化 | 画面静止 60 ms 后补发 4 帧（码率充足时逐帧变清晰） | 无 |
| 码率 | 较低的平均码率，允许峰值；自适应策略默认"画质优先" | 较高的恒定码率；自适应策略默认"均衡"（§6.2） |
| 鼠标 | 绝对坐标 | 可切换为相对坐标（锁定光标） |

第一阶段先实现 4:2:0 基线（M1），4:4:4 与静止细化在 M5 实现。办公模式只在客户端能硬件解码 4:4:4 时使用 4:4:4（客户端在 `ClientCaps` 中如实上报）。

### 3.3 客户端

```
QUIC 收帧 ──► 重组 ──► 解码线程（FFmpeg d3d11va）──► D3D11 纹理（NV12/YUV444）
                                                  ▼
                              渲染线程：Shader 转 RGB + 缩放 + 叠加本地光标
                                                  ▼
                              翻转交换链（FLIP_DISCARD，可等待对象，允许撕裂可选）
```

- **只呈现最新一帧**：解码输出队列长度为 1，旧帧直接丢弃，避免延迟累积。
- 解码失败或检测到帧号跳跃时：丢弃后续非关键帧，发送 `RequestKeyframe`。
- 缩放：窗口大小与远端分辨率不一致时由 Shader 双线性缩放；使用虚拟显示器时远端分辨率跟随窗口，1:1 显示（§14.1）。
- D3D11VA 初始化失败（例如云电脑没有硬件解码）时回退到软件解码。
- D3D11VA 没有、但 NVIDIA 的 NVDEC 有的格式（典型是 HEVC 4:4:4，较早驱动在 DXVA 下不提供）：客户端启动时用 CUDA 驱动的 `cuvidGetDecoderCaps` 问出 NVDEC 支持的格式，在 `ClientCaps` 里报为硬解，解码走 FFmpeg 的 `*_cuvid` 解码器（帧在 CPU 内存，再上传成纹理）。没有 NVIDIA 驱动时什么也不报；NVDEC 解码出错时和其他硬解一样改用软件解码并告知被控端。统计面板显示"硬解（NVDEC）"。

### 3.4 光标
- helper 从 DXGI 取得光标形状（彩色 / 单色 / 掩码三种），转换为 RGBA 位图，按内容哈希分配 ID，同一形状只传一次。
- 位置与可见性单独上报。
- 客户端**本地绘制光标**：绝对坐标模式下，客户端自己的鼠标位置即时生效，彻底消除光标延迟。

### 3.5 多显卡与笔记本（混合显卡）

#### 常见拓扑

| 拓扑 | 显示器接在哪 | 典型机器 |
|---|---|---|
| A | 独显 | 台式机只用独显 |
| B | 独显和核显各接一部分显示器 | 台式机两者都启用 |
| C | 内屏在核显；外接口可能在核显或独显 | 大多数游戏本 / 轻薄本（Optimus 混合模式） |
| D | 运行时可在核显和独显之间切换 | 带 MUX 独显直连 / Advanced Optimus 的游戏本 |

#### 三条原则
1. **采集必须在"显示器所在的 GPU"上做**：DXGI 桌面复制不支持跨 GPU（在其他 GPU 上调用 `DuplicateOutput` 会返回 `DXGI_ERROR_UNSUPPORTED`）。
2. **编码可以在任意 GPU 上做**。
3. **所有 D3D 设备都按适配器 LUID 显式创建**，不依赖系统默认 GPU，也不受 Windows"图形首选项"设置影响。

#### GPU 拓扑探测
helper 启动时（以及拓扑变化时）：
1. 用 `IDXGIFactory6` 枚举所有适配器及其输出，得到"显示器 → 采集 GPU"映射。
2. 对每块 GPU 通过 FFmpeg 实际打开一次编码器，探测能力：NVIDIA → NVENC，Intel → QSV；记录支持的编码格式、色度（4:2:0 / 4:4:4）和最大分辨率。
3. 通过 `Welcome` 把显示器列表和各自可用的编码能力告诉客户端。

#### 每个显示器一条独立管线
```
采集 GPU：DXGI 复制 → 颜色转换（NV12 / YUV444）
                │
                ├─ 编码 GPU 相同 ──► 直接编码（零拷贝）
                │
                └─ 编码 GPU 不同 ──► 跨 GPU 传输 ──► 编码 GPU 上编码
```
颜色转换总在采集 GPU 上完成：NV12 只有 BGRA 数据量的 37.5%，可以减少跨 GPU 传输量。

#### 编码 GPU 选择策略
| 配置 `encoder_preference` | 行为 |
|---|---|
| `auto`（默认） | 同一 GPU 上的编码器能满足当前模式要求（格式、色度、分辨率）时，就用同 GPU 编码；否则选能满足要求的其他 GPU（NVENC 优先）并走跨 GPU 传输 |
| `nvenc` / `qsv` | 强制使用指定编码器；不可用时回退到 `auto` 并在日志和客户端浮层中提示 |

例如：游戏本内屏在核显时，办公模式下核显支持 HEVC 4:4:4（Gen11+）就用 QSV 同 GPU 编码，不支持就用独显 NVENC 跨 GPU；用户也可以强制 `nvenc`。

#### 跨 GPU 传输（分两级实现）

| 方案 | 做法 | 开销（估算，待实测） | 阶段 |
|---|---|---|---|
| **T1 内存中转** | 采集 GPU 转换完成 → 拷到 staging 纹理 → 用事件查询等待拷贝完成后立即 `Map`（不做多帧流水，避免多一帧延迟）→ 上传到编码 GPU 的纹理 | 1080p NV12 约 3 MB/帧，增加约 1–2 ms；4K 约 12 MB/帧，增加约 3–5 ms | 第一阶段 |
| **T2 D3D12 跨适配器共享** | 用 `D3D12_HEAP_FLAG_SHARED_CROSS_ADAPTER` 的共享堆，两块 GPU 自己通过 PCIe 拷贝，CPU 只等待 | 更低，且不占 CPU | 已实现（`nya-win::transfer12`），能建立时优先使用 |

T2 的做法：采集 GPU 上把转换好的帧拷进一张与 D3D12 共享的纹理（NT 句柄）；采集 GPU 的 D3D12 复制队列把各平面拷进跨适配器堆里的缓冲（`GetCopyableFootprints` 布局），然后在跨适配器共享的 fence 上发信号；编码 GPU 的复制队列等这个 fence，再把缓冲拷进它那边与 D3D11 共享的纹理，CPU 等本地 fence 后用 D3D11 拷进编码器的输入表面。复制队列上的资源从 COMMON 隐式提升、执行完再退回 COMMON，不需要屏障。`GpuToGpu` 先尝试 T2，建立失败或运行中出错就永久改用 T1。`nya-server diag` 分别测 T1、T2 的耗时。

#### 拓扑变化与动态重建
以下情况会触发重建：
- 拓扑 D 的 MUX 切换；
- 插拔显示器；
- 驱动更新或重置；
- `DXGI_ERROR_ACCESS_LOST` / `DXGI_ERROR_DEVICE_REMOVED`；
- `IDXGIFactory::IsCurrent()` 返回 false；
- helper 隐藏窗口收到 `WM_DISPLAYCHANGE`。

处理流程：重新探测拓扑 → 重建受影响显示器的管线 → 发送 `DisplayChanged` 和新关键帧。目标是客户端**只感到 1 秒以内的卡顿**，连接不断。（实现中拓扑变化通过 `IDXGIFactory::IsCurrent()` 轮询和复制 / 编码失败触发，没有单独监听 `WM_DISPLAYCHANGE`。）

某个编码方案在运行中连续出错时，本次运行自动换用下一个方案（例如 NVENC → 软件编码）。

#### 客户端侧
客户端也可能是双显卡笔记本：
- 解码和渲染设备选**窗口所在显示器的 GPU**，避免跨 GPU 呈现。
- 该 GPU 不支持某种解码格式（如 HEVC 4:4:4）时，在 `ClientCaps` 中如实上报，由服务端降级。
- 窗口拖到另一块 GPU 的显示器上时，重建解码和渲染设备，并请求关键帧。

### 3.6 HDR 桌面
- 被控端显示器开启 HDR 时，DXGI 复制得到 FP16 scRGB 画面。默认由颜色转换 shader 做色调映射，以 Windows "SDR 内容亮度"（`DISPLAYCONFIG_SDR_WHITE_LEVEL`）为白点转换到 SDR，再走正常的 4:2:0 / 4:4:4 编码。客户端看到的和 SDR 显示器上一样，`StreamStarted.hdr_tonemapped` 告知客户端。
- **HDR10 直通**（`FEATURE_HDR`，协议 1.4）：客户端窗口所在显示器开着 HDR、设置里允许时，在 `StreamConfig.hdr` 里请求；`ClientCaps` 里 `CodecCap.ten_bit` 表示能解 HEVC Main10（硬解或软解）。被控端桌面是 HDR 时，选择方案把"采集 GPU 上的 HEVC 4:2:0 + HDR"排在最前（NVENC / QSV / AMF；P010 不跨显卡），失败再退回 SDR 方案。
  - 被控端：转换 shader 把 scRGB 从 BT.709 转到 BT.2020，乘 80 得到绝对亮度，PQ 编码，再按 BT.2020 非恒定亮度矩阵、10 bit 有限范围写进 P010（Y / UV 两个渲染目标）；码流标记 BT.2020 / SMPTE 2084，Main10 profile，自动码率多给 25 %。运行中 HDR 被关掉时，8 bit 桌面按 203 nit（BT.2408 参考白）放进 PQ。
  - 客户端：解码出 P010（硬解）或 yuv420p10（软解），渲染 shader 支持 10 bit 偏移与 BT.2020 矩阵。窗口所在显示器开着 HDR 时，交换链切到 FP16 scRGB（`RGB_FULL_G10_NONE_P709`），PQ 解码后按绝对亮度输出；SDR 视频和界面按该显示器的 SDR 白输出，界面先画到 8 bit 图层再合成。显示器不是 HDR（例如窗口拖到别的屏幕）时，在本机把 HDR10 色调映射到 SDR（203 nit 为白）。窗口移动和每秒检查一次显示器的 HDR 状态。
  - 统计面板显示"HDR10 直通"；`nya-server diag` 列出各编码器能否打开 HDR10。

---

## 4. 输入

| 事件 | 编码方式 | 注入方式 |
|---|---|---|
| 鼠标绝对移动 | 坐标归一化到 0–65535（相对目标显示器） | `SendInput` + `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` |
| 鼠标相对移动 | dx, dy | `SendInput` 相对移动（游戏模式） |
| 按键 | **扫描码** + 扩展位 + 按下/抬起 | `SendInput` + `KEYEVENTF_SCANCODE` |
| 滚轮 | 垂直/水平，单位 1/120 | `SendInput` |
| Ctrl+Alt+Del | 专用消息 | service 调用 `SendSAS` |

- 使用扫描码而不是虚拟键码：与两端键盘布局和输入法无关，游戏兼容性最好。
- **防止按键卡住**：helper 记录当前按下的键；断线、helper 重启、客户端失焦时，全部补发"抬起"。
- **桌面切换**：注入前检查 `OpenInputDesktop`，桌面变化时（Default ↔ Winlogon）调用 `SetThreadDesktop` 切换；同时重建 DXGI 复制对象（会收到 `DXGI_ERROR_ACCESS_LOST`）。
- 客户端系统快捷键（Win、Alt+Tab 等）：捕获键盘时通过低级键盘钩子拦截并转发；热键 `Ctrl+Alt+Shift+Q` 捕获 / 释放键盘。云电脑里低级钩子收不到按键（输入全部是注入的），所以同时从窗口的键盘事件转发。
- 手柄：客户端读取 XInput 手柄，经输入流发送；被控端通过 ViGEmBus 创建虚拟 Xbox 360 手柄并回传震动（§14.6）。

---

## 5. 音频

- 被控端：WASAPI **进程回环采集**，排除被控端自身的进程树（自己往 VB-Cable 播放的麦克风声音不会再传回客户端）；老系统不支持时回退到默认设备回环。48 kHz 立体声 → Opus（10 ms 帧，128 kbps，低延迟模式）。没有声音时 WASAPI 不产生数据，也就不发包。
- 传输：QUIC **不可靠数据报**，`u8 类型 | u32 序号 | u64 发送端时间戳(µs) | Opus 包`。
- 客户端：Opus 解码 → **自适应抖动缓冲**（`nya-media::jitter`）→ WASAPI 共享模式播放（设备内再保持约 20 ms）。

抖动缓冲（被控端播放麦克风时用的是同一个）：

| 方面 | 做法 |
|---|---|
| 目标深度 | 最近 10 秒内包到达延迟（到达时刻减去该包在媒体时间线上的位置）的散布，取 P97，加 5 ms 余量，限制在 20–80 ms；初始 30 ms。升高立即生效，降低每秒最多 2 ms，偶发一次延迟不会让延迟长期偏高 |
| 时钟漂移、向新目标靠拢 | 平滑后的缓冲深度偏离目标超过 4 ms 时，按偏差微调播放速度（最多 ±0.5 %，Catmull-Rom 三次插值重采样），回到 1 ms 以内后恢复原样直通，不丢样本 |
| 欠载 | 缓冲和设备都空了才算欠载，重新攒够目标深度后再播放 |
| 积压过多 | 网络卡顿后成批到达、超出目标 40 ms 以上时，直接丢弃到目标深度 |
| 丢包 | 序号缺口不超过 5 个包时补同样长度的静音（尚未用 Opus PLC）；乱序晚到和重复的包丢弃 |
| 被控端停发 / 重启 | 发送端时间戳跳变超过包长 50 ms 视为对方暂停发送（不补静音、不计入抖动）；序号倒退 50 个以上视为对方重新开始 |

统计面板显示缓冲深度 / 目标、抖动、当前调速和累计断音次数（§8）。

麦克风（第三阶段，已实现）：客户端 WASAPI 采集 → Opus → `MIC` 数据报 → 被控端经抖动缓冲播放到 VB-Cable 的 "CABLE Input"，麦克风打开期间被控端把 "CABLE Output" 设为默认录音设备（所有角色，经 IPolicyConfig），停止 3 秒后恢复原来的默认设备；原设备记在 `mic-default.txt`，helper 异常退出后下次启动时恢复。默认播放设备就是 VB-Cable 且只能用设备回环时，为避免回声会暂停麦克风并提示。

---

## 6. 网络传输

### 6.1 连接与认证
1. 被控端首次启动时生成自签名证书与**配对密钥**（随机 32 字节，显示为易读的配对码）。
2. 客户端首次连接时输入 `IP:端口` 和配对码；TLS 握手后，客户端通过控制流发送 `HMAC(配对密钥, 服务端证书指纹 ‖ 随机数)` 证明身份。
3. 成功后客户端**保存服务端证书指纹**（此后校验指纹，防中间人），服务端保存客户端证书指纹（此后免配对码）。
4. 默认端口 `UDP 47100`；可配置只绑定组网网卡的地址。

### 6.2 QUIC 通道划分

| 通道 | QUIC 形式 | 方向 | 内容 |
|---|---|---|---|
| 控制 | 双向流 #0（可靠有序） | 双向 | 握手、能力协商、配置变更、关键帧请求、Ping、统计、剪贴板 |
| 输入 | 单向流（可靠有序） | C→S | 键鼠事件 |
| 视频 | 每个推流会话（stream_id）一条单向流 | S→C | 开头：流类型 + stream_id（+ 槽位 slot，多画面时）；之后每帧 `u32 长度 + 帧头 + 码流` |
| 视频（数据报） | 数据报（不可靠）+ 纠错 | S→C | 游戏模式默认使用（`FEATURE_VIDEO_DATAGRAM`），每帧切成分片并加 Reed-Solomon 校验分片（§6.5） |
| 光标 | 单向流（可靠有序） | S→C | 形状、位置、可见性 |
| 文件 | 每批文件一条单向流 | 双向 | `FileHeader` + 文件内容（文件传输、剪贴板图片、粘贴时拉取的文件） |
| 隧道 | 双向流 | 双向 | 被控端发起，开头写端口号，之后是原始字节（USB/IP） |
| 音频 | 数据报（不可靠） | S→C | Opus 包 |
| 麦克风 | 数据报（不可靠） | C→S | Opus 包，格式同音频 |

- **每条单向流开头先写一个 varint"流类型"**；数据报第一个字节是"数据报类型"。收到不认识的类型时：流直接 `STOP_SENDING`，数据报直接丢弃。这样新版本可以增加通道而不影响旧版本。
- 视频默认按会话一条流、按序可靠：编码后的帧不丢弃，流控靠"已交给 QUIC 的帧确认"（FrameSent，最多 2 帧在途），拥塞时在采集端少编帧而不是丢帧，避免参考帧断裂。代价是丢一个包要等重传（至少一个 RTT），后面的帧都跟着卡。游戏模式默认改走数据报 + 纠错（§6.5）。
- MTU：初始 1200，开启 PMTU 探测；组网环境下（1280）也能正常工作。
- 拥塞控制：quinn 默认算法 + 应用层**码率自适应**（已实现，`server/src/abr.rs`）。主要信号是**积压**：已交给 QUIC 但还没发出去的视频字节。积压持续增长说明产出多于链路能承载，此时按实测发送速率设定新码率，而不是盲目按比例下调。RTT 增长和丢包在中转 / 代理路径上噪声很大，只在更激进的策略中使用。客户端可选策略：

| 策略 | 触发条件 | 持续时间 | 码率下限 |
|---|---|---|---|
| 画质优先（办公默认） | 积压 > 1 s | 2 s | 60 % |
| 均衡（游戏默认） | 积压 > 400 ms，或排队延迟 | 0.75 s | 35 % |
| 流畅优先 | 积压 > 200 ms，排队延迟或丢包 | 0.5 s | 15 % |
| 固定 | 不调整 | – | 100 % |

  多路画面时，自动码率在各路之间分配。

### 6.3 消息定义（草案）

控制消息与输入消息使用 **Protobuf**（varint 长度前缀分帧，单条上限 1 MiB）。以下为最初的草案，只用于说明结构；**字段以 `nya-proto/proto/nya.proto` 为准**（例如认证改为双向 HMAC：`AuthResult.server_mac` 让客户端也能确认服务端持有配对密钥）。

```proto
syntax = "proto3";
package nya.v1;

// ===== 握手（字段号永久冻结，任何版本之间都必须能完成这一步）=====
message Hello {                       // 客户端在控制流上发送的第一条消息
  uint32 proto_major = 1;
  uint32 proto_minor = 2;
  uint32 min_proto_major = 3;         // 客户端还能兼容的最低 MAJOR
  string client_name = 4;
  string client_version = 5;          // 软件版本，仅用于显示和日志
  repeated uint32 features = 6;       // Feature 编号；用 uint32 以便旧版本收到未知编号时不出错
  // 客户端证书指纹直接取自 TLS 握手，不在消息里重复
}
message HelloReply {
  oneof reply {
    Welcome welcome = 1;
    Reject  reject  = 2;
  }
}
message Reject {
  RejectReason reason = 1;            // VERSION_TOO_OLD / VERSION_TOO_NEW / AUTH_FAILED / BUSY ...
  string message = 2;                 // 可直接展示给用户的说明
  uint32 server_proto_major = 3;
  uint32 server_min_proto_major = 4;
}
message Welcome {
  uint32 proto_major = 1;             // 本次会话使用的 MAJOR
  uint32 proto_minor = 2;             // 双方 MINOR 的较小值
  string server_name = 3;
  string server_version = 4;
  repeated uint32 features = 5;       // 双方功能的交集 = 本次会话启用的功能
  repeated DisplayInfo displays = 6;  // 含每个显示器所在 GPU 与可用编码能力
  bool   needs_pairing = 7;           // 为 true 时接下来进行配对认证
}

enum Feature {
  FEATURE_UNSPECIFIED = 0;
  AUDIO = 1;  LOCAL_CURSOR = 2;  CLIPBOARD_TEXT = 3;  YUV444 = 4;
  STATIC_REFINE = 5;  SAS = 6;  MULTI_GPU_INFO = 7;
  // 以后新增：FILE_MOUNT、MIC、USB、GAMEPAD、VIDEO_DATAGRAM_FEC ...
}

// ===== 握手之后的控制消息 =====
message ControlMsg {
  oneof msg {
    AuthChallenge   auth_challenge   = 1;
    AuthResponse    auth_response    = 2;
    ClientCaps      client_caps      = 3;
    StartStream     start_stream     = 4;
    StreamStarted   stream_started   = 5;   // 服务端实际采用的配置
    SetMode         set_mode         = 6;   // OFFICE / GAME
    RequestKeyframe request_keyframe = 7;
    DisplayChanged  display_changed  = 8;
    Ping            ping             = 9;
    Pong            pong             = 10;
    ClientStats     client_stats     = 11;
    ClipboardText   clipboard_text   = 12;
    SendSas         send_sas         = 13;  // Ctrl+Alt+Del
    Bye             bye              = 14;
  }
}

message CodecCap    { Codec codec = 1; Chroma chroma = 2; uint32 max_width = 3; uint32 max_height = 4; }
message StreamConfig{ Codec codec = 1; Chroma chroma = 2; uint32 width = 3; uint32 height = 4;
                      uint32 fps = 5; uint32 bitrate_kbps = 6; StreamMode mode = 7; }

// ===== 输入（输入流上的消息）=====
message InputMsg {
  oneof ev {
    MouseAbs    mouse_abs    = 1;   // display_id, x/y 归一化到 0–65535
    MouseRel    mouse_rel    = 2;
    MouseButton mouse_button = 3;
    Wheel       wheel        = 4;
    Key         key          = 5;   // scancode, extended, down
    ReleaseAll  release_all  = 6;
  }
}
```

视频帧头（手写小端二进制，紧跟码流；**带版本和长度，可以在末尾追加字段**）：

```
u8  header_version  当前为 1
u8  header_len      头部总字节数（v1 = 28）；解析时按此长度跳到码流，未知的尾部字段直接忽略
u16 flags           bit0 关键帧, bit1 配置变更, bit2 静止细化帧；未知位忽略
u64 frame_id        递增；用于检测跳帧
u64 capture_ts_us   服务端采集时刻（结合 Ping 估算的时钟偏差计算端到端延迟）
u16 width, u16 height
u8  codec, u8 chroma
u16 reserved
```

### 6.4 协议兼容策略

**版本号：`MAJOR.MINOR`**
- **同一 MAJOR 下必须能互通**。MINOR 升级只允许"增加"：加字段、加消息、加功能、加通道类型。
- MAJOR 只在确实无法兼容时才升级。服务端同时实现**当前 MAJOR 和上一个 MAJOR**，按握手结果分派到对应的处理模块。
- 超出兼容范围时返回 `Reject`，并明确告诉用户"请升级客户端"或"请升级被控端"，而不是直接断开。

**功能协商优先于版本判断**
- 新行为一律用 `Feature` 开关控制：双方都声明支持才启用。
- 代码里不写 `if minor >= 3` 这类判断。
- 例如：旧客户端不声明 `YUV444`，服务端就只给它发 4:2:0。

**修改 .proto 的规则**（代码评审时检查）
1. 字段号和字段类型永远不改；删除的字段写进 `reserved`。
2. 新字段不出现时，默认值（0 / 空 / false）必须等价于"旧行为"。
3. 枚举新增值时，收到未知值的一方必须有安全的默认处理；跨版本的集合型数据（如 `features`）用 `uint32` 传输。
4. `Hello` / `HelloReply` / `Reject` 的已有字段永久冻结。

**收到"不认识的东西"时的处理**
| 情况 | 处理 |
|---|---|
| 未知的 oneof 分支（新消息） | 忽略，记 debug 日志 |
| 未知字段 | Protobuf 自动忽略 |
| 未知流类型 | `STOP_SENDING` |
| 未知数据报类型 | 丢弃 |
| 帧头变长、未知 flags 位 | 按 `header_len` 跳过、忽略未知位 |

**协议版本记录**

| 版本 | 新增内容 | 状态 |
|---|---|---|
| 1.0 | 第一阶段（M0–M7） | 已发布、已冻结 |
| 1.1 | 文件传输、剪贴板图片、麦克风、USB 透传、手柄、码率策略、`ServerStats.target_kbps` | 已发布、已冻结 |
| 1.2 | 虚拟显示器 / 隐私屏（`DisplaySetup`）、剪贴板文件、多画面（`slot`）、多客户端（`SessionRole` / `TakeControl`）、HDR 标记 | 已发布（server / client 0.2.0、0.3.0）、已冻结 |
| 1.3 | `ServerStats.encode_ms_p99` | 已发布（server / client 0.4.0）、已冻结 |
| 1.4 | 视频数据报 + 纠错（`FEATURE_VIDEO_DATAGRAM`、`StreamConfig.video_transport`、`ClientStats` 分片统计、`ServerStats.fec_percent`）；文件夹挂载（`FEATURE_FOLDER_MOUNT`、`SharedFolders` / `FolderMountStatus`、FS 流与 `FsRequest` / `FsReply`）；打印到客户端（`FEATURE_PRINT`、`FilePurpose.PRINT`）；HDR10 直通（`FEATURE_HDR`、`StreamConfig.hdr`、`CodecCap.ten_bit`） | 已发布（server / client 0.5.0）、已冻结 |
| 1.5 | 文字输入（`FEATURE_TEXT_INPUT`、`InputMsg.text`）：客户端输入的文字由被控端以 Unicode 打入，与被控端键盘布局、输入法无关（手机直接输入中文） | 已发布（server 0.6.0、Android 0.1.0）、已冻结 |

**兼容性测试**
- 每次发布，把 `.proto` 冻结一份到 `nya-proto/proto/history/vX.Y/`，并用 `NYA_BLESS=1 cargo test -p nya-proto --test compat` 生成 `tests/compat/vX.Y/`。发版脚本（`release-lib.ps1` 的 `Test-NyaProtoFrozen`）会检查当前协议版本已冻结且与冻结的 `.proto` 一致，否则拒绝发版。
- 开发中第一次给协议加字段时就把 `PROTO_MINOR` 加一；冻结的版本不能再改。
- 自动测试：用历史版本编码的样例消息，当前代码必须能解码；另外测未知字段、未知 oneof 分支、未知枚举值，以及握手协商的结果（版本、功能交集、Reject 原因）。（"当前代码编码的消息用历史 schema 解码"尚未自动化，靠 Protobuf 规则和代码评审保证。）
- 发布前手动跑一遍"上一版客户端 ↔ 新服务端"和"新客户端 ↔ 上一版服务端"两组连接测试。

### 6.5 视频数据报与纠错（FEC）

`FEATURE_VIDEO_DATAGRAM`（协议 1.4）。客户端在 `StreamConfig.video_transport` 里选：自动（默认：游戏模式走数据报，办公模式走流）/ 可靠流 / 数据报。被控端按主窗口（slot 0）的请求和当前模式决定，模式切换时随之切换，并请求关键帧（两条路径上的帧可能乱序到达）。

| 方面 | 做法（`nya-transport::videodgram`） |
|---|---|
| 分片 | 每帧（帧头 + 码流）按当前 `max_datagram_size` 切成等长分片（最后一片可短），每片一个数据报，带 32 字节头：slot、分片大小、stream_id、frame_id、帧长、原始 / 校验分片数、序号 |
| 纠错 | Reed-Solomon（`reed-solomon-simd`，O(n log n)，单帧可到上万分片，不用再分组）。校验分片数 = 原始分片数 × 纠错比例，至少 2 片；先发原始分片，再发校验分片，不丢包时收齐原始分片就能出帧，不需要解码 |
| 重组 | 任意 `原始分片数` 个分片到齐即可还原。较新的一帧完成时，还没凑齐的旧帧直接放弃（解码器必须按顺序），解码器发现帧号跳跃后请求关键帧，和其他丢帧的处理一样 |
| 纠错比例 | 默认 20 %，范围 10–50 %。客户端每秒在 `ClientStats` 里报告分片收到 / 丢失、纠错恢复 / 丢帧数，被控端按 `10 % + 3 × 丢包率` 立即调高，有丢帧再加 10 %，网络干净时每秒降 1 % |
| 流控与码率 | 数据报同样受 QUIC 拥塞控制；`send_datagram_wait` 在发送缓冲满时等待，FrameSent 在一帧的分片全部交给 QUIC 后发出，积压照常计入码率自适应。校验分片额外占用 10–50 % 带宽 |
| 统计 | `ServerStats.fec_percent`（0 = 走流）；统计面板显示"传输 数据报 + 纠错 N %、丢包率、纠错恢复帧数、丢帧数" |

尚未做：丢帧后用 NVENC 的参考帧失效 / 帧内刷新代替整帧关键帧（目前请求关键帧，关键帧较大，弱网下恢复要稍久）。

---

## 7. 进程与线程模型

### 7.1 被控端 helper
| 线程 | 职责 |
|---|---|
| 采集编码线程（高优先级，MMCSS "Games"/"Capture"） | AcquireNextFrame → 转换 → 编码 → 写入发送队列 |
| 音频线程（MMCSS "Pro Audio"） | WASAPI 回采 → Opus |
| 输入线程 | 从 IPC 接收输入 → 桌面检查 → SendInput |
| IPC 线程（tokio） | 与 service 收发，发送队列满时丢弃非关键帧 |

### 7.2 被控端 service
- tokio 运行时：QUIC 连接、IPC 转发、会话事件处理（`SERVICE_CONTROL_SESSIONCHANGE`）。
- 会话切换流程：收到事件 → 结束旧 helper → 在 `WTSGetActiveConsoleSessionId()` 会话中启动新 helper → 通知客户端 `StreamStarted`（新关键帧）。

### 7.3 IPC（service ↔ helper）
- 命名管道 `\\.\pipe\nya-rc-<随机后缀>`，DACL 仅允许 SYSTEM；后缀通过命令行参数传给 helper。
- 帧格式：`u32 长度 + u8 类型 + 负载`。视频码率在 100 Mbps 以内时命名管道足够；以后需要时再改为共享内存环形缓冲。

### 7.4 客户端
| 线程 | 职责 |
|---|---|
| UI / 事件循环（winit，主线程） | 窗口、输入采集 |
| 网络（tokio） | QUIC 收发、流分发 |
| 解码线程 | FFmpeg d3d11va |
| 渲染线程 | 等待交换链 → 取最新帧 → 绘制 + 光标 → Present |
| 音频线程 | 抖动缓冲 → WASAPI |

---

## 8. 延迟目标（局域网，1080p60）

| 环节 | 目标 |
|---|---|
| 采集（AcquireNextFrame 返回后） | ≤ 1 ms |
| 颜色转换 + 编码 | ≤ 5 ms（NVENC）/ ≤ 8 ms（QSV） |
| IPC + 网络 | ≤ 3 ms |
| 解码 | ≤ 4 ms |
| 渲染到 Present | ≤ 1 帧 |
| **合计（不含显示器）** | **≤ 25 ms** |

客户端统计面板（热键 `Ctrl+Alt+Shift+S`）每秒刷新，多画面时每路一块：编码器与分辨率、帧率（被控端 / 本机）、丢帧、码率（实际 / 自适应上限）与策略说明、端到端延迟和 RTT、编码 / 解码 / 渲染耗时、跨显卡传输耗时、解码器，以及声音缓冲（深度 / 目标、抖动、调速、断音次数）。耗时和延迟显示**本秒中位数和最近 10 秒的 P99**（`nya-proto::stats::Rolling`；一秒只有几十帧，算不出有意义的 P99）。旧版被控端不报编码 P99，显示"—"。所有性能调优以这组数据为依据。

---

## 9. 配置、日志与安全

- 被控端配置：服务模式 `%ProgramData%\NyaRemoteControl\server.toml`（端口、绑定地址、名称、编码器偏好、码率、帧率、声音、日志级别、自动检查更新），开发模式 `%LOCALAPPDATA%\NyaRemoteControl\server\`；证书与已配对客户端列表存于同目录，ACL 仅 SYSTEM / Administrators。
- 客户端配置：`%APPDATA%\NyaRemoteControl\client\client.toml`：本机名称、默认设置 `[defaults]`、主机列表 `[[hosts]]`（地址、证书指纹、被控端自报名称、可选的单独设置 `[hosts.settings]`）。连接后在工具条里改的模式、显示器设置、码率策略、麦克风、键盘捕获自动记到该主机的单独设置。
- 日志：`tracing` 输出到文件，按天滚动，保留 7 天。
- 安全要点：
  - 所有网络输入**先校验长度再解析**，限制单条消息大小。
  - 未通过认证的连接只能发送握手消息，超时 10 秒断开。
  - service 以 SYSTEM 运行，攻击面仅限 QUIC 端口，且只有已配对客户端能进入会话。

---

## 10. 风险与待验证项

| # | 风险 | 应对 / 验证方式 |
|---|---|---|
| R1 | 客户端 D3D11VA 是否支持 **HEVC 4:4:4 解码**（NVIDIA 驱动在 DXVA 下的支持需实测） | M5 前实测；不支持则回退到 FFmpeg `hevc_cuvid`（NVDEC）或 4:2:0 |
| R2 | Intel QSV 与 D3D11 纹理互操作（FFmpeg `qsv` 从 `d3d11va` 派生设备） | M5 用核显机器实测 |
| R3 | 双显卡笔记本：T1 内存中转在 4K 高帧率下的开销 | M6 实测；不满意时提前实现 T2（D3D12 跨适配器共享） |
| R3b | MUX 切换、插拔显示器时的重建速度和稳定性；各厂商混合显卡驱动的行为差异 | M6 在游戏本上反复切换、插拔测试 |
| R4 | 独占全屏游戏 / 受 DRM 保护的内容无法采集或黑屏 | DRM 内容属于系统限制，接受；独占全屏游戏在 M1 实测 |
| R5 | FFmpeg 在 Windows 上的构建与链接（Rust 绑定版本匹配） | M0 先打通最小示例 |
| R6 | quinn 默认拥塞控制对实时视频不友好（码率骤降、排队） | 第二阶段实现应用层码率自适应，必要时评估 BBR |
| R7 | helper 以 SYSTEM 身份运行时 WASAPI 回采的行为 | M4 实测（Sunshine 已验证可行） |
| R8 | 系统快捷键拦截与输入法交互 | M2 实测 |
| R9 | 虚拟显示器驱动（MttVDD）模式过多时显示器无法接入（约 100 个模式以上 `IddCxMonitorArrival` 失败） | 已规避：只写 60 Hz + 客户端刷新率，最多 64 个模式；排查用 `nya-server-svc vdd-test --driver-log` |
| R10 | 剪贴板文件：被控端资源管理器（普通用户）回调 SYSTEM 身份 helper 的 OLE 数据对象 | 依赖 `CoInitializeSecurity` 放开交互用户调用；被控端粘贴失败时先查这里 |
| R11 | 自动更新后服务没有恢复，被控端失联 | 独立更新程序：备份 → 安装 → 等新服务应答 → 失败则回滚，最后总是确保服务在运行（§14.8）；需实机演练 |
| R12 | 文件夹挂载：WinFsp FUSE 接口是按其头文件手写的绑定（结构布局有单元测试核对），开发机没装 WinFsp，挂载本身未实测 | 实机安装 WinFsp 后验证；失败时查服务日志里的 mount / folder request 记录 |
| R13 | HDR10 直通：客户端 HDR 输出（FP16 交换链、界面合成）只在没有 HDR 显示器的机器上做过编译和数学测试 | 有 HDR 显示器后实测；设置里可关掉"HDR 直通"退回原来的 SDR 传输 |

---

## 11. 第一阶段里程碑

| 里程碑 | 内容 | 验收标准 |
|---|---|---|
| **M0** 工程骨架 | workspace、crate 划分、FFmpeg 链接、日志、配置；`.proto` 与版本/功能协商、兼容性测试框架；GPU 拓扑枚举工具 | 两个 bin 能编译运行；能列出所有 GPU、显示器及各自可用的编码器；握手协商单元测试通过 |
| **M1** 画面打通 | standalone：DXGI → Shader → NVENC H.264/HEVC 4:2:0 → QUIC → d3d11va → 呈现 | 局域网 1080p60 稳定，浮层显示端到端延迟 ≤ 30 ms |
| **M2** 输入与光标 | 键鼠注入、本地光标绘制、防卡键、快捷键拦截 | 日常操作、打字、拖拽无异常 |
| **M3** 声音 | WASAPI → Opus → 数据报 → 播放 | 看视频时声画基本同步（偏差 < 80 ms） |
| **M4** 服务化 | service + helper、会话切换、锁屏/登录/UAC、Ctrl+Alt+Del、断线重连 | 远程锁屏 → 输入密码登录 → 注销 → 重新登录全程可操作 |
| **M5** 编码完善 | QSV 路径、编码器探测与自动选择、4:4:4 办公模式、静止细化、模式切换 | 核显机器可用；办公模式下文字清晰度肉眼接近本地 |
| **M6** 多显卡与笔记本 | 按显示器建管线、编码 GPU 选择策略、T1 跨 GPU 传输、拓扑变化时动态重建、客户端按窗口所在 GPU 解码 | 游戏本（内屏在核显）上 QSV 同 GPU 和 NVENC 跨 GPU 两种方式都可用；MUX 切换或插拔外接屏后 1 秒内恢复画面 |
| **M7** 可用性 | 配对流程、剪贴板文本、主机列表、统计浮层完善；跨版本连接测试 | 可日常自用 |

M0–M7 的代码均已完成；实机验证情况见 §13。

---

## 12. 后续阶段规划与完成情况

**第二阶段：顺手**

| 项目 | 状态 |
|---|---|
| 码率自适应 | ✅ 已实现（§6.2） |
| 多显示器切换 / 同时显示 | ✅ 已实现，同时显示为多窗口（§14.2） |
| 远端分辨率跟随客户端窗口（虚拟显示器） | ✅ 已实现，另有隐私屏（§14.1） |
| 文件拖拽传输、剪贴板图片 / 文件 | ✅ 已实现（§14.4） |
| 跨 GPU 传输 T2（D3D12 跨适配器共享） | ✅ 已实现（§3.5）：建立不了或运行中出错时自动改用 T1 |
| 文件夹挂载（WinFsp） | ✅ 已实现（§14.5） |

**第三阶段：接近云电脑**

| 项目 | 状态 |
|---|---|
| 麦克风（VB-Cable） | ✅ 已实现（§5） |
| USB 重定向（usbip-win2） | ✅ 已实现（§14.6） |
| 手柄（ViGEmBus） | ✅ 已实现（§14.6） |
| AMD AMF 编码 | ✅ 已接入（按显卡厂商选择，未在 AMD 显卡上实测） |
| Android 客户端 | ✅ 已实现（§14.9），未在手机上实测 |
| 打印（虚拟 PDF 打印机回传） | ✅ 已实现（§14.6） |
| 游戏模式弱网：视频走数据报 + FEC | ✅ 已实现（§6.5） |

**其他待做**
- 丢包时用 Opus PLC 代替补静音（§5）。

---

## 13. 实现状态与实机验证清单

开发机只做编译和纯 CPU / 本机回环测试（按约定不在开发机上测试硬件）。测试机：被控端为公司台式机（GTX 1650），客户端为天翼云电脑（无硬件解码，走软件解码）。

| 已在实机上验证 | 代码完成、尚未实机验证 |
|---|---|
| 服务模式安装运行（v0.1.0-alpha2） | 声音（含 SYSTEM 身份回采、R7）、新的自适应抖动缓冲 |
| 锁屏截屏，远程输入 PIN 解锁 | 剪贴板文字 / 图片 / 文件（R10）、文件传输 |
| NVENC HEVC 4:2:0 编码，客户端软件解码 | 游戏模式、相对鼠标、4:4:4（需硬解客户端） |
| 显示器切换、键盘输入 | 断线重连、Ctrl+Alt+Del、注销 / 切换用户 |
| 虚拟显示器在被控端出现（R9 修复后） | 隐私屏、分辨率跟随窗口、多虚拟屏、HDR 转 SDR |
| | 多窗口、多客户端观看 / 接管、客户端同时连多台 |
| | 麦克风、手柄、USB 透传 |
| | 视频数据报 + 纠错（游戏模式默认，§6.5） |
| | 文件夹挂载（WinFsp 驱动加载、盘符出现在用户会话、资源管理器读写、大文件速度） |
| | 打印到客户端（添加打印机、文件端口写入权限、客户端打印效果） |
| | NVDEC 解码回退（需要客户端有 N 卡；现有客户端是云电脑，没有） |
| | 跨显卡传输 T2（需要双显卡的被控端；现有被控端只有一块 GTX 1650） |
| | HDR10 直通（被控端 GTX 1650 的 Main10 编码可用 `diag` 验证；客户端 HDR 显示需要 HDR 显示器，现有测试机没有） |
| | 管理程序 + 控制管道（服务模式下的管道权限） |
| | 安装包升级、自动更新与回滚（R11；0.2.0 安装的被控端没有更新程序，第一次需手动升级） |
| | QSV、AMF、跨显卡传输（需要对应硬件） |
| | Android 客户端全部功能（§14.9；开发机没有模拟器）；USB 透传为自写的 USB/IP 服务端，最需要实测 |

| | 文字输入（`KEYEVENTF_UNICODE`，协议 1.5） |

自动测试（CPU / 回环）覆盖：协议编解码与跨版本解码、版本协商；QUIC 连接、证书指纹锁定、配对 HMAC；编码方案选择逻辑；端到端网络协议（配对、推流、流控、输入回传、Ping、多客户端、游戏模式视频走数据报并还原）；视频分片与纠错（丢分片恢复、丢帧、乱序、重复、换流、丢包统计）；码率自适应策略；Opus 编解码、OpenH264 编码→软件解码；抖动缓冲（稳态直通、抖动、时钟漂移、丢包、暂停、积压）；着色器编译、YUV→RGB 矩阵、光标形状转换。

实机验证步骤：

1. 被控端运行 `nya-server diag`，把 `nya-diag.txt` 发回来；
2. 用 `nya-server-svc standalone` 加 `nya-client connect <ip>` 验证画面、键鼠、声音；
3. 用安装包（或 `nya-server install`）安装服务，验证锁屏、注销、UAC 和 Ctrl+Alt+Del；
4. 虚拟显示器问题用 `nya-server-svc vdd-test --driver-log`（管理员）收集信息。

---

## 14. 第一阶段之后新增的设计

### 14.1 虚拟显示器与隐私屏
- 使用 Virtual Display Driver（MttVDD，可选组件一键安装）。设备平时停用，有客户端要求时才启用；客户端断开 15 秒后停用并恢复 Windows 原来的显示器布局（避免网络闪断时来回切换）；helper 异常退出后，下次启动先清理残留状态。
- 客户端通过 `StartStream.display_setup`（`FEATURE_VIRTUAL_DISPLAY`）描述想要的样子：1–4 个虚拟屏（第一个设为主显示器，分辨率 / 刷新率 / 缩放由客户端给出）、是否关闭物理显示器、是否屏蔽被控端本地键鼠。三项合起来即"隐私屏"。用 CCD API 设置布局；屏蔽本地输入用低级键鼠钩子（远程注入的输入不受影响；本地的 Ctrl+Alt+Del 无法屏蔽）。
- 分辨率：跟随窗口 / 跟随本机屏幕 / 固定。驱动只认 `C:\VirtualDisplayDriver\vdd_settings.xml` 里列出的模式，新尺寸需要改文件并重启驱动（约 2 秒），之后切换即时；模式数受 R9 限制。
- 需要服务模式；条件不满足时提示原因并改用物理显示器。

### 14.2 多画面（多窗口）
- `FEATURE_MULTI_STREAM`：每个客户端窗口一个槽位（slot，0 = 主窗口），每个槽位一条视频流、被控端一条管线。`RequestKeyframe`、`StopStream`、`ServerStats`、`MouseAbs` 都带 slot。
- 客户端可以为某个显示器开新窗口，或"新建虚拟屏并在新窗口打开"；每个虚拟屏的分辨率跟随显示它的窗口（`DisplayInfo.virtual_index` 对应关系）。窗口可以放在本机同一块屏幕上（例如 4K 屏放两三个）。
- 关闭额外窗口只停那一路；主窗口断开结束整个连接。

### 14.3 多客户端
- `FEATURE_MULTI_CLIENT`：第一个连上的客户端操作，之后的客户端观看（只收主画面 slot 0，键鼠、剪贴板、USB、显示设置都只听操作者的）。观看者可以"接管操作"（原操作者转为观看）或"顶掉对方"（原操作者断开）；操作者断开后最早的观看者自动接管。`SessionRole` 告诉每个客户端自己的角色、操作者和观看者名单。
- 服务端新增"影响被控端"的请求时必须检查操作者身份（`server/src/net.rs` 的 `in_control`）。
- 不支持该功能的旧客户端连上时仍然顶掉所有人；同一客户端重连时替换自己的旧会话。
- 客户端可同时连接多台不同的被控端，每台一个窗口；同一台不能连两次。

### 14.4 文件与剪贴板
- 文件传输：拖进窗口或菜单发送，保存到被控端当前用户的 `下载\NyaRemoteControl` 并放入剪贴板；被控端也可以向客户端提供文件。
- 剪贴板文字、图片双向自动同步。
- 剪贴板文件（`FEATURE_CLIPBOARD_FILES`，含文件夹）：复制时只发送文件列表（`FileOffer`），在另一边放一个 OLE 虚拟 `CF_HDROP` 数据对象；真正粘贴时才通过文件流拉取到粘贴缓存（客户端 `%LOCALAPPDATA%\NyaRemoteControl\clipboard`，一天后清理），再交给资源管理器复制。连接断开后对方复制的文件不能再粘贴。

### 14.5 文件夹挂载（WinFsp）
- `FEATURE_FOLDER_MOUNT`：客户端在设置里选要共享的文件夹（可设只读），连接后发 `SharedFolders`；被控端用 WinFsp（可选组件，签名驱动）的 FUSE 接口挂到一个空闲盘符（从 Z: 往前找），每个共享文件夹是盘根下的一个目录，结果用 `FolderMountStatus` 告诉客户端。
- 每个文件系统调用 = 被控端打开一条 FS 双向流，写一个 `FsRequest`，读一个 `FsReply`（`nya-transport::folders`）。请求无状态（每次带路径，不保持打开的句柄），连接断开不会在客户端留下东西。单次读写最多 512 KiB；WinFsp 缓存文件和目录信息 1 秒，资源管理器浏览不会每步都等网络。
- 安全：客户端只回答共享文件夹之内的路径（拒绝 `..`、盘符、反斜杠），只读文件夹拒绝一切修改，共享文件夹本身不能被删除或改名；跨共享文件夹的移动被拒绝（资源管理器会改用复制 + 删除）。
- 只有操作者的文件夹会挂载：观看者的请求被记下，接管操作时再挂；失去操作权、清空列表或断开时卸载。
- 服务以 SYSTEM 运行，盘符在全局命名空间，登录用户能看到；文件权限给 Everyone（0777），是否允许修改由客户端决定。
- WinFsp 在运行时按注册表里的安装目录加载 `winfsp-x64.dll`，没装时被控端照常工作，只是告诉客户端"没有安装 WinFsp"。

### 14.6 外设与可选组件
- 可选组件（虚拟显示器、VB-Cable、ViGEmBus、usbip-win2、WinFsp、打印到客户端；客户端侧 usbipd-win）由管理界面 / 客户端一键安装：下载地址和 SHA-256 固定，走系统代理，也可以使用随安装包附带的 `drivers` 离线目录。
- 手柄：XInput → `InputMsg.gamepad` → ViGEm 虚拟 Xbox 360 手柄，震动经 `GamepadRumble` 回传。
- 打印到客户端（`FEATURE_PRINT`）：可选组件"打印到客户端"用 Windows 自带的"Microsoft Print To PDF"驱动加一台打印机"打印到 NyaRemoteControl 客户端"，端口是数据目录下的文件 `print\job.pdf`（给 Users 修改权限，后台打印程序可能以打印用户的身份写入）。服务每秒检查一次：文件写完（大小非零、1.5 秒没变、能独占打开）就改名为"被控端打印 <本地时间>.pdf"，交给操作者的会话用文件流（`FilePurpose.PRINT`）发出，发完删除；没人接收的文件保留一天。客户端存到 `下载\NyaRemoteControl\打印`，按设置直接用默认打印机打印（Windows.Data.Pdf 逐页渲染，最高 300 dpi，GDI 按可打印区域等比居中）、打开 PDF 或只保存；打印失败时改为打开 PDF。只在服务模式下工作。
- USB：客户端 usbipd-win 共享设备，被控端 usbip-win2 连接；USB/IP 的 TCP 流量经 QUIC 隧道流转发（§6.2），被控端在 127.0.0.1:3240 监听，代替客户端的 usbipd 让 usbip-win2 连接。

### 14.7 界面
- 客户端主界面（设备列表、连接设置、关于与诊断）和被控端管理界面（概览、已配对客户端、设置、可选组件、诊断、日志）都是 `common/web` 里的 Svelte 页面，由 `nya-webui` 用 WebView2 显示，通过 JSON 调用 / 事件与 Rust 通信。`npm run dev` 可在浏览器里用示例数据预览。
- 远程画面在单独的会话窗口（D3D11 交换链），会话工具条和统计面板用 egui 绘制在上面。
- 注意：创建 winit 窗口前不能在 UI 线程初始化多线程 COM，否则 winit 的 OleInitialize 失败。

### 14.8 安装、发布与自动更新
- 版本：server、client 各自的 `VERSION` 文件（语义化版本），和 Cargo.toml 保持一致；显示的版本带提交号。`scripts/release.ps1 x.y.z` 改版本、写 `COMMON_REF`、提交并打 tag；推送 tag 后 GitHub Actions 构建 NSIS 安装包、便携 zip 和 `.sha256` 并发布到 Releases（带后缀的为预发布）。发版前检查 common 已推送、协议已冻结（§6.4）。
- 被控端更新：服务每 12 小时检查 GitHub Releases。确认更新后，服务下载安装包并核对 SHA-256，再启动独立于服务的更新程序（`%ProgramData%\NyaRemoteControl\update\nya-updater.exe`，即 `nya-server-svc.exe` 的副本，`apply-update` 子命令）：备份当前文件 → 静默安装 → 等待新版本服务在 120 秒内通过控制管道应答 → 不正常则恢复备份并重新注册服务 → 无论如何最后确保服务在运行。结果写入 `update\result.json`，服务下次启动时报告。
- 客户端更新：启动时检查；更新时以管理员权限静默运行安装包后退出，安装完成后经 explorer.exe 以普通用户身份重新打开。便携版只打开发布页。

### 14.9 Android 客户端
- 独立仓库 `android/`（GitHub `NyaRemoteControl-android`），单独发版。界面是 Kotlin + Jetpack Compose；协议部分是 Rust 核心 `libnya_android.so`（JNI），直接使用 common 的 nya-proto、nya-transport、nya-jitter，和 Windows 客户端同一套协议代码。Gradle 构建时用 cargo-ndk 编译核心（arm64-v8a、x86_64）。
- 分工：核心负责连接、握手、配对、两分钟内自动重连、视频数据报还原、丢帧后等关键帧（并请求）、统计、文件收发、文件夹请求、USB/IP；Kotlin 线程以阻塞调用拉取事件（JSON）、视频帧、音频包，输入直接推送。核心声明除 4:4:4 以外的全部功能（手机解码器只有 4:2:0）：声音、本地光标、剪贴板文字 / 图片 / 文件、静止细化、Ctrl+Alt+Del、虚拟显示器、多画面前缀、多客户端、视频数据报、文件传输、手柄、文字输入、麦克风、文件夹挂载、打印、HDR、USB 透传。
- 视频：MediaCodec 硬件解码直接输出到 SurfaceView，开低延迟参数和各厂商的低延迟开关（不认识时去掉重试），解码出一帧立即显示。只上报硬件解码器（模拟器上允许软件 H.264）。
- HDR：手机屏幕支持 HDR10、有硬件 HEVC Main10 解码器、设置里开着 HDR 时，上报 `CodecCap.ten_bit` 并请求 `StreamConfig.hdr`；被控端回复 HDR10 时解码器按 BT.2020 / PQ 配置，由系统以 HDR 显示。
- 显示器：面板列出被控端的显示器（`SessionInfo.displays`），切换时重发 `StartStream`（`display_id`），虚拟屏保留。
- 分辨率：默认在被控端建一块和手机屏幕一样大的虚拟屏（横屏物理像素，缩放默认 150%），也可选不超过 1080p 或被控端原分辨率；被控端没有虚拟显示器时自动用物理显示器。画面可以双指缩放、平移，软键盘弹出时可把画面下部推到键盘上方。
- 操作：触屏式（点哪里操作哪里：单击、长按右键、长按拖动、单指滚动、三指键盘）和鼠标式（触控板：相对移动光标、双指滚动、双指轻按右键），双指捏合缩放画面；连接时显示手势指引。悬浮球打开侧边面板（操作方式、键盘、快捷键、办公 / 游戏模式、统计、剪贴板、发送文件、接管、断开）。实体键盘、鼠标、手柄（最多 4 个，震动回传）可直接使用。
- 键盘：始终是普通文本输入框，手机输入法（拼音、手写、语音）都能用；拼写中的内容留在输入法里，只发送提交的结果。被控端支持文字输入（协议 1.5）时以 `InputMsg.text` 发送；旧被控端上，美式键盘能打出的字走扫描码，中文等经被控端剪贴板（`ClipboardText`）再按 Ctrl+V（会覆盖被控端剪贴板）。组合键（锁定了修饰键时）总是走扫描码，换行和 Tab 作为按键。按键和粘贴经同一个队列按顺序发出。键盘上方有附加键栏，Ctrl / Alt / Shift / Win 锁定到下一个键。也可以切换为屏幕上的电脑布局键盘（设置里选默认，键盘上随时切换，记住选择）：按下发送按下、松开发送松开，修饰键锁定到下一个键，退格 / 删除 / 方向键按住自动重复，画面可推到键盘上方。三指轻按打开当前选择的键盘。
- 声音：MediaCodec 解 Opus，PCM 进核心的自适应抖动缓冲（与 Windows 相同），浮点 AudioTrack 低延迟播放，设备里保持约 20 ms。
- 文件：手机选文件（系统文件选择器）后由核心经文件流发送；被控端复制文件时手机提示"保存到手机"，收到后存入 `下载/NyaRemoteControl`。
- 剪贴板：文字双向；图片双向（CF_DIB，手机端转成 PNG 经 FileProvider 放进剪贴板，发送时缩到每边不超过 4096 像素）；手机剪贴板里的文件（content URI）先复制到缓存再按剪贴板文件提供给被控端，在电脑上粘贴时才传输（`FEATURE_CLIPBOARD_FILES`，与 Windows 客户端同一套 `nya-transport::clipfiles`）。面板"发送剪贴板"把手机剪贴板（文字 / 图片 / 文件）发给电脑；被控端的文字和图片按设置自动进入手机剪贴板。
- 打印：被控端的打印任务（PDF）到手机后提示"打印 / 保存"，打印走系统打印框架（已安装的打印服务或另存 PDF）。
- 麦克风：AudioRecord 48 kHz 单声道复制成双声道，MediaCodec Opus 编码（Android 10 以上），每包一个 MIC 数据报；需要被控端装有虚拟声卡（`SessionInfo.mic_device`）。面板开关，记住上次状态。
- 文件夹挂载：设置里选择手机文件夹（系统选择器，转换为存储路径，可设只读），连接时发 `SharedFolders`，核心直接用 `nya-transport::folders` 回答被控端的 FS 请求。需要"所有文件访问"权限（Android 11 以上）或存储权限（更早的版本）。
- USB 透传：手机 OTG 接口上的设备在面板里选择共享。应用申请权限、打开设备并强制占用所有接口，把文件描述符和原始描述符交给核心；核心自己就是 USB/IP 服务端（代替 Windows 上的 usbipd-win），在被控端经隧道流连来时回答设备列表 / 导入，把 URB 通过 usbdevfs 异步提交（SUBMITURB / REAPURBNDELAY / DISCARDURB），控制传输里的 SET_CONFIGURATION / SET_INTERFACE / CLEAR_FEATURE(HALT) 改用对应 ioctl。不支持等时传输（摄像头、声卡类设备），这类请求回错误。
- 应用内更新：每天最多自动检查一次 GitHub Releases（设置里可手动检查），下载 APK 并核对 `.sha256`，交给系统安装程序。
- 签名与发布：密钥由 `scripts/new-android-keystore.ps1` 生成在仓库外的 `signing/`，写入仓库的 Actions secrets；推送 `v*` 标签构建签名 APK 发布到 Releases。CI 在每次推送时跑 Rust 测试（含假被控端回环测试：配对、推流、关键帧请求、文字输入、双向文件、共享文件夹；USB/IP 服务端用模拟设备测设备列表、导入、提交、取消）、Kotlin 单元测试、lint，并产出 debug APK。
- 不做：多画面窗口（用显示器切换代替）、4:4:4。

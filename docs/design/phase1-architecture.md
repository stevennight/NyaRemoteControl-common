# NyaRemoteControl 架构设计（第一阶段）

| 项目 | 内容 |
|---|---|
| 状态 | v0.3：M0–M7 代码已实现并通过 CPU / 回环测试，待在实际机器上验证（见 §13）。v0.2：协议兼容、多显卡与笔记本；v0.3：按实现修订 §2、§5、§6 |
| 日期 | 2026-09-27 |
| 范围 | 整体架构 + 第一阶段（Windows ↔ Windows 可用版）详细设计；第二、三阶段只做规划 |

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
- 不做多用户、多会话（一台被控机同一时刻一个控制者）。
- 不支持 RDP 会话内运行（只服务控制台会话）。
- 第一阶段不做 HDR、AMD、Android、外设重定向。

### 目标环境
- 被控端：Windows 10 21H2+ / Windows 11，x64，NVIDIA（GTX 10 系及以上）或 Intel 核显（Gen9+，HEVC 4:4:4 需 Gen11+）。
- 客户端：Windows 10/11 x64（第一阶段）。
- 网络：局域网或组网虚拟网（MTU 可能低至 1280）。

---

## 1. 总体架构

```
┌──────────────────────────── 被控端 ────────────────────────────┐
│                                                                │
│  nya-server.exe service   （Session 0，SYSTEM，Windows 服务）    │
│   ├─ QUIC 监听、认证、连接管理（网络只在这里）                   │
│   ├─ 监控会话切换（登录/注销/锁屏/切换用户）                      │
│   ├─ 在活动控制台会话中拉起 / 重启 helper                        │
│   └─ 发送 Ctrl+Alt+Del（SendSAS）                               │
│            ▲  命名管道（仅 SYSTEM 可访问）                        │
│            ▼                                                    │
│  nya-server.exe helper    （活动控制台会话，SYSTEM 令牌）          │
│   ├─ 画面：DXGI 桌面复制 → 颜色转换 → NVENC/QSV 编码             │
│   ├─ 光标：形状 + 位置单独上报                                   │
│   ├─ 声音：WASAPI 回采 → Opus                                    │
│   ├─ 输入：SendInput 注入（跟随输入桌面切换）                     │
│   └─ 剪贴板                                                      │
└────────────────────────────────────────────────────────────────┘
                         │  QUIC（UDP，TLS 1.3）
                         │  经由外部组网工具
┌──────────────────────── 客户端 ────────────────────────────────┐
│  nya-client.exe                                                 │
│   ├─ 网络：QUIC 连接、流分发                                      │
│   ├─ 视频：D3D11VA 硬件解码 → Shader 转 RGB → 翻转交换链呈现      │
│   ├─ 光标：本地绘制（零延迟）                                     │
│   ├─ 声音：Opus 解码 → 抖动缓冲 → WASAPI 播放                    │
│   ├─ 输入：键鼠采集（扫描码），发送                               │
│   └─ 统计浮层：帧率 / 码率 / 各环节延迟                           │
└────────────────────────────────────────────────────────────────┘
```

### 1.1 为什么拆成 service + helper 两个进程
| 问题 | 解决 |
|---|---|
| 服务运行在 Session 0，**看不到用户桌面**，无法采集画面和注入输入 | helper 运行在活动控制台会话内 |
| 普通用户进程**无法访问 Winlogon 桌面**（锁屏、登录界面、UAC 安全桌面） | helper 使用 **SYSTEM 令牌**（复制服务令牌并改 SessionId）启动 |
| 用户注销 / 切换用户时，会话内进程会被销毁或换会话 | 网络连接放在 service 中，helper 重启时**连接不断**，客户端只会感到短暂卡顿 |
| Ctrl+Alt+Del 无法通过 SendInput 模拟 | service 调用 `SendSAS`（需开启策略 `SoftwareSASGeneration`） |

service 与 helper 是**同一个可执行文件的不同子命令**，便于部署。

### 1.2 开发模式
开发阶段提供 `nya-server.exe standalone`：单进程、普通用户权限、直接监听网络、不涉及服务与会话切换。里程碑 M1–M3 全部在此模式下完成，M4 再接入 service/helper。

---

## 2. 代码仓库结构

三个独立仓库放在同级目录，共享一个 `target/`；FFmpeg 放在不属于任何仓库的 `third_party/`：

```
NyaRemoteControl/
├─ common/   仓库：公共 crate（下方结构）与本文档
├─ server/   仓库：nya-server（service / helper / standalone / install / diag）
├─ client/   仓库：nya-client（Windows 客户端）
└─ third_party/ffmpeg/   FFmpeg 8.1 LGPL 预编译包（common/scripts/fetch-ffmpeg.ps1 下载）
```

common 仓库内部：

```
common/
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ nya-proto/              # 协议：.proto 定义、帧头、版本与功能协商；无平台依赖（Android 共用）
│  │  ├─ proto/               #   当前 .proto
│  │  ├─ proto/history/       #   每个发布版本冻结的 .proto 快照（兼容性测试用）
│  │  └─ tests/compat/        #   新旧版本互解码测试
│  ├─ nya-transport/          # QUIC 封装：握手、认证、流/数据报分发（Android 共用）
│  ├─ nya-media/              # FFmpeg 封装：编码器/解码器抽象、能力探测；Opus 封装
│  ├─ nya-win/                # Windows 平台层：D3D11 设备、DXGI 复制、颜色转换、
│  │                          #   SendInput、WASAPI、桌面切换、服务/令牌/命名管道
│  ├─ nya-server/             # bin：service / helper / standalone
│  └─ nya-client/             # bin：Windows 客户端
├─ docs/
│  └─ design/
└─ (后续) crates/nya-android/ + android/   # JNI cdylib + Kotlin 工程
```

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
| 帧率 | 最高 60，按变化驱动 | 固定目标帧率（60/120/144），稳定输出 |
| 静止细化 | 画面静止 150 ms 后补发一帧**低 QP 高质量帧** | 无 |
| 码率 | 较低的平均码率，允许峰值 | 较高的恒定码率 |
| 鼠标 | 绝对坐标 | 可切换为相对坐标（锁定光标） |

第一阶段先实现 4:2:0 基线（M1），4:4:4 与静止细化在 M5 实现。

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
- 缩放：窗口大小与远端分辨率不一致时由 Shader 双线性缩放；第二阶段支持"远端跟随窗口调整分辨率"。

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
| **T2 D3D12 跨适配器共享** | 用 `D3D12_HEAP_FLAG_SHARED_CROSS_ADAPTER` 的共享堆和行主序纹理，由编码 GPU 通过 PCIe 直接读取，CPU 不参与 | 更低，且不占 CPU | 第二阶段（T1 实测不满意时做） |

#### 拓扑变化与动态重建
以下情况会触发重建：
- 拓扑 D 的 MUX 切换；
- 插拔显示器；
- 驱动更新或重置；
- `DXGI_ERROR_ACCESS_LOST` / `DXGI_ERROR_DEVICE_REMOVED`；
- `IDXGIFactory::IsCurrent()` 返回 false；
- helper 隐藏窗口收到 `WM_DISPLAYCHANGE`。

处理流程：重新探测拓扑 → 重建受影响显示器的管线 → 发送 `DisplayChanged` 和新关键帧。目标是客户端**只感到 1 秒以内的卡顿**，连接不断。

#### 客户端侧
客户端也可能是双显卡笔记本：
- 解码和渲染设备选**窗口所在显示器的 GPU**，避免跨 GPU 呈现。
- 该 GPU 不支持某种解码格式（如 HEVC 4:4:4）时，在 `ClientCaps` 中如实上报，由服务端降级。
- 窗口拖到另一块 GPU 的显示器上时，重建解码和渲染设备，并请求关键帧。

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
- 客户端系统快捷键（Win、Alt+Tab 等）：全屏且获得焦点时，通过低级键盘钩子拦截并转发；提供一个热键（默认 `Ctrl+Alt+Shift+Q`）释放键盘。

---

## 5. 音频

- 被控端：WASAPI **回环采集**默认播放设备，48 kHz 立体声 → Opus（10 ms 帧，128 kbps，低延迟模式）。
- 传输：QUIC **不可靠数据报**（丢包由 Opus PLC 补偿）。
- 客户端：抖动缓冲（目标 30 ms，自适应 20–80 ms）→ WASAPI 共享模式播放。
- 时钟漂移：缓冲水位过高时丢弃样本，过低时插入静音或 PLC 帧（第一阶段），后续改为微调重采样。
- 第一阶段不做麦克风（第三阶段通过 VB-Cable 实现）。

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
| 视频 | 每个推流会话（stream_id）一条单向流 | S→C | 开头：流类型 + stream_id；之后每帧 `u32 长度 + 帧头 + 码流` |
| 光标 | 单向流（可靠有序） | S→C | 形状、位置、可见性 |
| 音频 | 数据报（不可靠） | S→C | Opus 包 |

- **每条单向流开头先写一个 varint"流类型"**；数据报第一个字节是"数据报类型"。收到不认识的类型时：流直接 `STOP_SENDING`，数据报直接丢弃。这样新版本可以增加通道而不影响旧版本。
- 视频按会话一条流、按序可靠：编码后的帧不丢弃，流控靠"已交给 QUIC 的帧确认"（FrameSent，最多 2 帧在途），拥塞时在采集端少编帧而不是丢帧，避免参考帧断裂。游戏模式在弱网下的"数据报 + FEC"方案放到第三阶段。
- MTU：初始 1200，开启 PMTU 探测；组网环境下（1280）也能正常工作。
- 拥塞控制：第一阶段使用 quinn 默认算法 + 固定码率；第二阶段加入基于 RTT / 丢包 / 解码队列反馈的**码率自适应**。

### 6.3 消息定义（草案）

控制消息与输入消息使用 **Protobuf**（varint 长度前缀分帧，单条上限 1 MiB）。以下为草案：

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

**兼容性测试**
- 每次发布，把 `.proto` 冻结一份到 `nya-proto/proto/history/vX.Y/`。
- 自动测试：用历史版本 schema 编码的消息，当前代码必须能解码；当前代码编码的消息，历史 schema 也必须能解码。另外还要测握手协商的结果（版本、功能交集、Reject 原因）。
- 发布前手动跑一遍"上一版客户端 ↔ 新服务端"和"新客户端 ↔ 上一版服务端"两组连接测试。

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

客户端统计浮层（默认热键 `Ctrl+Alt+Shift+S`）显示：帧率、码率、RTT、各环节耗时 P50/P99、丢帧数、当前编码器与模式。所有性能调优以这组数据为依据。

---

## 9. 配置、日志与安全

- 被控端配置：`%ProgramData%\NyaRemoteControl\server.toml`（端口、绑定地址、编码器偏好、默认码率）；证书与已授权客户端列表存于同目录，ACL 仅 SYSTEM / Administrators。
- 客户端配置：`%APPDATA%\NyaRemoteControl\client.toml`（主机列表、证书指纹、快捷键）。
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

---

## 12. 后续阶段规划（概要）

**第二阶段：顺手**
- 码率自适应、多显示器切换 / 同时显示
- 跨 GPU 传输 T2（D3D12 跨适配器共享，视 M6 实测结果决定是否提前）
- 远端分辨率跟随客户端窗口（借助 Virtual Display Driver 创建虚拟显示器，被控机不接显示器也能用）
- 文件夹挂载（WinFsp 用户态文件系统，I/O 请求经新 QUIC 通道转发到客户端）
- 文件拖拽传输、剪贴板图片 / 文件

**第三阶段：接近云电脑**
- Android 客户端（`nya-android` JNI + Kotlin UI + MediaCodec；只协商 4:2:0；触控手势映射）
- 麦克风（客户端采集 → 写入被控端 VB-Cable）
- 打印（虚拟 PDF 打印机 → 文件回传 → 客户端本地打印）
- USB 重定向（集成 usbip-win2）
- 手柄（ViGEmBus 虚拟手柄）
- 游戏模式弱网优化：视频改走数据报 + FEC
- AMD AMF 编码

---

## 13. 实现状态与实机验证清单

M0–M7 的代码已全部实现。开发机上只做了编译和纯 CPU / 本机回环测试（按约定不在开发机上测试硬件），**所有硬件相关功能都需要在实际机器上验证**：

| 已自动测试（CPU / 回环） | 需要实机验证 |
|---|---|
| 协议编解码、版本协商、跨版本解码（19 项） | DXGI 截屏、NV12/AYUV 渲染目标 |
| QUIC 连接、证书指纹锁定、配对 HMAC | NVENC / QSV 打开、4:4:4、编码延迟 |
| 编码方案选择逻辑（笔记本、强制 NVENC、软件回退） | 跨显卡传输耗时（R3） |
| 端到端网络协议：配对、推流、流控、输入回传、Ping | D3D11VA 解码，HEVC 4:4:4 硬解（R1） |
| Opus 编解码、OpenH264 编码→软件解码 | 服务模式：锁屏 / 登录 / UAC / 注销 / Ctrl+Alt+Del |
| 着色器编译、YUV→RGB 矩阵、光标形状转换 | WASAPI 回采（SYSTEM 身份，R7）、键盘钩子、剪贴板 |

实机验证步骤：

1. 被控端先运行 `nya-server diag`，把 `nya-diag.txt` 发回来；
2. 用 `nya-server standalone` 加 `nya-client connect <ip>` 验证画面、键鼠、声音；
3. 再用 `nya-server install` 安装服务，验证锁屏、注销、UAC 和 Ctrl+Alt+D。

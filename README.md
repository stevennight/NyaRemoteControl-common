# NyaRemoteControl 公共库（common）

Windows 版（`../windows`：主程序和远程控制服务）和 Android 版（`../android`）共用的代码。

| crate | 内容 |
|---|---|
| `nya-proto` | 线协议：`proto/nya.proto`（Protobuf）、视频帧头、流/数据报类型、版本与功能协商、长度前缀读写、统计百分位 |
| `nya-transport` | QUIC（quinn）端点、自签名证书与指纹、配对码与 HMAC 证明 |
| `nya-ffmpeg-sys` | FFmpeg 8.1 的预生成绑定和链接；构建时把 DLL 复制到输出目录 |
| `nya-media` | 编码器（NVENC / QSV / AMF / OpenH264）、解码器（D3D11VA / 软件）、Opus、音频抖动缓冲 |
| `nya-win` | Windows 层：D3D11 设备、显卡拓扑、DXGI 截屏、颜色转换 shader、跨显卡拷贝、键鼠注入、桌面切换、WASAPI、显示配置、剪贴板 |
| `nya-ui` | egui on Direct3D 11（会话工具条、统计面板），带中文字体 |
| `nya-webui` | WebView2 宿主：嵌入 `web/` 构建出的页面，页面与 Rust 之间的调用和事件 |

`web/` 是 Windows 主程序的界面（Vite + Svelte 5）：`src/client` 控制别的电脑，`src/host` 是“本机”（被控）部分；`npm run dev` 用示例数据预览（地址加 `?page=host` 直接打开“本机”，再加 `&user` 看普通用户权限下的样子）。

设计文档见 [docs/design/phase1-architecture.md](docs/design/phase1-architecture.md)。

## 准备

```powershell
.\scripts\fetch-ffmpeg.ps1          # 下载 FFmpeg 到 ..\third_party\ffmpeg
cargo test                          # 所有测试只用 CPU，不需要显卡
```

## 协议兼容规则（摘要，完整内容见设计文档 §6.4）

- 同一 MAJOR 内必须能互通；MINOR 只允许"增加"字段、消息、功能和通道。
- 新行为一律通过 `Feature` 协商开启，代码里不比较版本号。
- 字段号和类型永远不改，删除的字段写进 `reserved`；`Hello` / `HelloReply` / `Welcome` / `Reject` 的已有字段永久冻结。
- 开发中**第一次**给协议加字段 / 消息时，把 `PROTO_MINOR` 加一，新字段注释里写 `since X.Y`。已冻结的版本不能再改。
- Windows 版或 Android 版发版、且其中的协议版本还没冻结时：
  1. 把 `proto/nya.proto` 复制到 `proto/history/vX.Y/`；
  2. 运行 `$env:NYA_BLESS=1; cargo test -p nya-proto --test compat`，生成 `tests/compat/vX.Y/`；
  3. 提交并推送 common，再运行发版脚本。

  发版脚本会检查这一点（`scripts/release-lib.ps1` 的 `Test-NyaProtoFrozen`）：当前版本没冻结，或 `nya.proto` 与冻结的不一致，都会拒绝发版。

## 重新生成 FFmpeg 绑定（升级 FFmpeg 时）

需要 libclang（例如 `pip install libclang`，然后把 `LIBCLANG_PATH` 设为 `site-packages\clang\native`）：

```powershell
.\scripts\gen-ffmpeg-bindings.ps1
```

同时要更新 `nya-ffmpeg-sys` 里的 `EXPECTED_AVCODEC_MAJOR` / `EXPECTED_AVUTIL_MAJOR`，并核对手写的 `AVD3D11VA*` / `AVQSVFramesContext` 结构体。

## Windows 版 / Android 版如何引用本仓库

目前通过同级目录的 `path` 依赖引用。以后有了远程仓库，可以改成 `git = "...", tag = "vX.Y.Z"`，本地开发时再用 `[patch]` 覆盖回本地路径。

## 仓库布局

本项目由三个仓库组成，需要克隆到同一个父目录下（windows / android 通过 `../common` 引用公共库）：

```powershell
gh repo clone stevennight/NyaRemoteControl-common common
gh repo clone stevennight/NyaRemoteControl-windows windows   # 主程序 + 远程控制服务（0.7.0 起由 server 和 client 合并而成）
gh repo clone stevennight/NyaRemoteControl-android android   # Android 版（只能控制别人）
```

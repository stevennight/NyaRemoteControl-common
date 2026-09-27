# NyaRemoteControl 公共库（common）

被控端（`../server`）和客户端（`../client`）共用的代码。

| crate | 内容 |
|---|---|
| `nya-proto` | 线协议：`proto/nya.proto`（Protobuf）、视频帧头、流/数据报类型、版本与功能协商、长度前缀读写 |
| `nya-transport` | QUIC（quinn）端点、自签名证书与指纹、配对码与 HMAC 证明 |
| `nya-ffmpeg-sys` | FFmpeg 8.1 的预生成绑定和链接；构建时把 DLL 复制到输出目录 |
| `nya-media` | 编码器（NVENC / QSV / AMF / OpenH264）、解码器（D3D11VA / 软件）、Opus |
| `nya-win` | Windows 层：D3D11 设备、显卡拓扑、DXGI 截屏、颜色转换 shader、跨显卡拷贝、键鼠注入、桌面切换、WASAPI、剪贴板 |

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
- 发布新版本时：
  1. 把 `proto/nya.proto` 复制到 `proto/history/vX.Y/`；
  2. 修改 `PROTO_MINOR` / `PROTO_MAJOR`；
  3. 运行 `$env:NYA_BLESS=1; cargo test -p nya-proto --test compat`，生成该版本的样例文件；
  4. 提交 `tests/compat/vX.Y/`。

## 重新生成 FFmpeg 绑定（升级 FFmpeg 时）

需要 libclang（例如 `pip install libclang`，然后把 `LIBCLANG_PATH` 设为 `site-packages\clang\native`）：

```powershell
.\scripts\gen-ffmpeg-bindings.ps1
```

同时要更新 `nya-ffmpeg-sys` 里的 `EXPECTED_AVCODEC_MAJOR` / `EXPECTED_AVUTIL_MAJOR`，并核对手写的 `AVD3D11VA*` / `AVQSVFramesContext` 结构体。

## 服务端 / 客户端如何引用本仓库

目前通过同级目录的 `path` 依赖引用。以后有了远程仓库，可以改成 `git = "...", tag = "vX.Y.Z"`，本地开发时再用 `[patch]` 覆盖回本地路径。

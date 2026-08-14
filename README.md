# MPV STT Plugin

MPV 实时字幕插件(单 crate,多 mod)。插件是**纯远程客户端**,通过 `[stt] backend`
配置在运行时选择后端,不内置任何本地推理引擎。

[![build](https://github.com/canxin121/mpv-subtitle-plugin/actions/workflows/build.yml/badge.svg)](https://github.com/canxin121/mpv-subtitle-plugin/actions/workflows/build.yml)

## 架构

### 仓库结构(单 crate)

```
mpv-subtitle-plugin/
├── Cargo.toml               # 单 crate (mpv-stt-plugin, cdylib+rlib)
├── build.rs                 # 链接处理(macOS dynamic_lookup / Windows FORCE:UNRESOLVED / Android -lmpv)
├── src/
│   ├── lib.rs               # mod 声明 + re-export
│   ├── common.rs            # 错误类型 / Result
│   ├── crypto.rs            # AES-256-GCM / AuthToken(ferrum 协议)
│   ├── srt.rs               # SRT 字幕解析/偏移
│   ├── audio.rs             # FFmpeg 音频抽取
│   ├── config.rs            # 配置 + 后端选择
│   ├── plugin.rs            # mpv cplugin 入口 (mpv_open_cplugin)
│   ├── ffi.rs               # C 导出(翻译 / 音频 / SRT)
│   ├── process.rs           # 子进程管理
│   ├── subtitle_manager.rs  # 字幕管理
│   ├── translate.rs         # 远程翻译客户端(DeepL 兼容 + LibreTranslate)
│   └── stt/
│       ├── mod.rs           # SttRunner / SttBackend 调度
│       ├── ferrum.rs        # ferrum 协议后端 (stt_ferrum)
│       └── openai.rs        # OpenAI 协议后端 (stt_openai)
├── third_party/rust-ffmpeg  # 子模块(vendored ffmpeg-next)
├── scripts/
│   ├── build-all.sh         # 全平台构建
│   ├── android-mpv/         # Android 交叉编译 mpv/ffmpeg
│   └── README.md
└── .github/workflows/build.yml  # CI
```

### 后端

插件支持两个远程 STT 后端,**同时编译、运行时选择**(`config.stt.backend`):

| Cargo feature | 配置字段 | 协议 |
|---|---|---|
| `stt_ferrum` | `[stt.ferrum]` | 自定义 ferrum 协议:raw-body POST `/transcribe`,支持 Opus 压缩 / AES-256-GCM 加密 / 鉴权 / 模型选择(`x-model`)/ 语言提示(`x-language`) |
| `stt_openai` | `[stt.openai]` | 标准 OpenAI `POST /v1/audio/transcriptions`(multipart),任何兼容服务端可用 |

ferrum 协议的服务端由 [subtitle-gateway](https://github.com/canxin121/subtitle-gateway)
(FunASR ASR + 翻译统一网关)实现,同一端点复用同一套 FunASR 引擎。

## 编译

### 系统依赖

桌面平台**动态链接**预编译 FFmpeg(**不再从源码编译**):

- macOS: `brew install ffmpeg`(插件链接其 dylib)
- Linux / Windows: 构建脚本自动下载 BtbN/FFmpeg-Builds 的 `lgpl-shared`
  预编译包到 `target/ffmpeg-btbn`,并复制运行时库到 `dist/<平台>/runtime/`

| 平台 | 依赖 |
|---|---|
| Ubuntu/Debian | `sudo apt-get install clang pkg-config`(clang 供 bindgen 用) |
| Arch Linux | `sudo pacman -S clang pkg-config` |
| macOS | `brew install ffmpeg`(自带 clang) |
| Windows | MSVC 工具链(BtbN 预编译共享包自动下载) |
| Android(交叉) | NDK + `meson ninja-build pkg-config cmake` |

**注意**: 不需要安装 `libmpv-dev`,构建脚本自动下载 mpv 头文件(bindgen 用)。

### 本地构建(宿主)

```bash
# 首次构建会自动 clone mpv 头文件到 target/mpv-headers
cargo build --release
cargo test

# 产物(以 macOS 为例;动态链接 brew ffmpeg,约 5MB)
ls target/release/libmpv_stt_plugin.dylib
```

> macOS 上若 mpv 头文件不在 `/opt/homebrew/include`,需
> `BINDGEN_EXTRA_CLANG_ARGS="-I/opt/homebrew/include" cargo build`。
> 桌面构建需要 `FFMPEG_DIR`(macOS 由脚本自动取 `brew --prefix ffmpeg`)。

### 全平台构建

```bash
# 全平台矩阵(桌面 4 平台 + Android 默认 arm64-v8a),每个平台/ABI 一个动态库(双后端编入)
# Linux/Windows 自动下载 BtbN 预编译共享包,macOS 用 brew ffmpeg
./scripts/build-all.sh

# 指定平台
./scripts/build-all.sh -p darwin-arm64

# Android(需 NDK;32 位 ABI 受上游 ffmpeg-sys-next 的 vulkan 假头 assert 阻塞,见下)
export ANDROID_NDK_HOME=~/Android/Sdk/ndk/26.1.10909125
./scripts/build-all.sh -p android -a arm64-v8a

./scripts/build-all.sh -l   # 列出支持的平台/feature/ABI
```

**支持平台**(CI 各平台原生编译;桌面全部动态链接 FFmpeg):

| 平台 | FFmpeg 来源 | 产物 |
|---|---|---|
| Linux x86_64 | BtbN `linux64-lgpl-shared`(自动下载) | `dist/linux-x86_64/plugin/libmpv_stt_plugin.so` |
| macOS arm64 | brew ffmpeg | `dist/darwin-arm64/plugin/libmpv_stt_plugin.dylib` |
| macOS x86_64 | brew ffmpeg | `dist/darwin-x86_64/plugin/libmpv_stt_plugin.dylib` |
| Windows x86_64 | BtbN `win64-lgpl-shared`(自动下载) | `dist/windows-x86_64/plugin/mpv_stt_plugin.dll` |
| Android(arm64-v8a,默认) | Android 侧动态 libffmpeg(随 APK 分发) | `dist/android/<abi>/plugin/libmpv_stt_plugin.so` |

Linux / Windows 产物旁的 `dist/<平台>/runtime/` 里是插件需要的 FFmpeg 运行时库
(`.so` / `.dll`),分发时需一起带上。macOS 插件直接链接 brew 的 dylib(安装 ffmpeg 即可)。

> **注**: `armeabi-v7a` / `x86`(32 位 ABI)暂未纳入默认构建——上游
> `ffmpeg-sys-next` 的 Vulkan stub 头硬编码 `sizeof(VkPhysicalDeviceFeatures2) == 240`
> (仅 64 位指针成立),bindgen 在 32 位目标上会失败。等上游修复后再放开。

产物默认**双后端编入**(单个动态库),运行时 `config.stt.backend` 切换;需要单后端
专用构建时加 `-f stt_ferrum` / `-f stt_openai`。

**CI**: push 到 `main` 后 GitHub Actions 自动构建并上传 `dist-<platform>` /
`dist-android` artifacts。详见 [scripts/README.md](./scripts/README.md) 与
`.github/workflows/build.yml`。

### 安装

**插件**(用 `dist/<平台>/plugin/` 里的动态库;macOS 记得先 `brew install ffmpeg`):
```bash
# Linux / Android / Windows
cp dist/linux-x86_64/plugin/libmpv_stt_plugin.so ~/.config/mpv/scripts/
# macOS
cp dist/darwin-arm64/plugin/libmpv_stt_plugin.dylib ~/.config/mpv/scripts/
```

> Linux / Windows 若从 dist 分发,把 `dist/<平台>/runtime/` 里的 FFmpeg 库一起带上;
> Windows 需放到 DLL 搜索路径(mpv.exe 同目录或 PATH)。

### 配置

插件通过 `mpv_stt_plugin_rs.toml` 配置(路径见 mpv 的 `--no-config` 之外的加载逻辑,
通常为 `~/.config/mpv/mpv_stt_plugin_rs.toml`)。选择后端:

```toml
[stt]
backend = "openai"   # openai（默认） | ferrum

[stt.openai]
server_addr = "http://127.0.0.1:8000"
model = "sensevoice"
language = "ja"            # 可选语言提示 (ja/zh/en...); 省略 = 服务端自动检测
api_key = ""               # 可选; 设置后发 Authorization: Bearer {key} (通用 OpenAI 兼容服务)
timeout_ms = 120000
max_retry = 3

# [stt.ferrum]
# server_addr = "http://127.0.0.1:8000"
# model = "sensevoice"        # 通过 x-model header 传给服务端
# language = "ja"             # 可选语言提示, 通过 x-language header 传 (省略 = 自动检测)
# use_opus = true
# enable_encryption = false
# encryption_key = "..."
# auth_secret = "..."
```

### 翻译(远程)

翻译同样走**远程接口**——插件不再内置翻译逻辑(已移除对 Google 网页接口的直连),
而是调用 DeepL 兼容协议:`POST {server}/v1/translate`,鉴权头
`Authorization: DeepL-Auth-Key {key}`,body
`{"text": [...], "target_lang": "ZH", "source_lang": "EN"}`(source_lang 省略 = auto),
响应 `{"translations": [{"text": "..."}]}`。

```toml
[translate]
from_lang = "en"
to_lang = "zh"
concurrency = 4
server_addr = "http://127.0.0.1:8000"   # DeepL 兼容服务基址（subtitle-gateway 网关或任意上游）
api_key = ""                              # 可选：网关鉴权用 DeepL-Auth-Key
```

subtitle-gateway 提供该协议的服务端端点 `/v1/translate`,转发到配置的上游翻译服务
(官方 DeepL / 自建 DeepLX / 任意 DeepL 兼容):
```bash
./run.sh --port 8000 \
  --translate-upstream https://api-free.deepl.com \
  --translate-upstream-key <上游key> \
  --translate-api-key <网关key>     # 可选：客户端须带此 key
```

### 翻译双协议(DeepL 兼容 + LibreTranslate)

插件同时编译两种翻译协议,`[translate] backend` 运行时选择(对称于 `[stt] backend`):

| backend | 配置字段 | 协议 |
|---|---|---|
| `deepl`（默认） | `[translate]` 平铺 `server_addr`/`api_key` | `POST {server}/v1/translate`，key 走 `Authorization: DeepL-Auth-Key`，target 大写 |
| `libretranslate` | `[translate.libretranslate]` | `POST {server}/translate`，key 走 body `api_key`，target 小写，`auto` 可显式/省略 |

```toml
[translate]
backend = "deepl"            # deepl（默认）| libretranslate
from_lang = "en"
to_lang = "zh"
concurrency = 4
server_addr = "http://127.0.0.1:8000"   # DeepL 兼容基址
api_key = ""                            # DeepL-Auth-Key

[translate.libretranslate]
server_addr = "http://127.0.0.1:8000"   # LibreTranslate 兼容基址
api_key = ""                            # body api_key
```

subtitle-gateway 同时提供两个协议网关端点 `/v1/translate`（DeepL）与 `/translate`
（LibreTranslate），转发到各自配置的上游：
```bash
./run.sh --port 8000 \
  --translate-upstream https://api-free.deepl.com \
  --translate-upstream-key <deepl上游key> \
  --translate-api-key <deepl网关key> \
  --libretranslate-upstream http://127.0.0.1:5000 \
  --libretranslate-upstream-key <libre上游key> \
  --libretranslate-api-key <libre网关key>
```

## Features

- `stt_ferrum` (default): 自定义 ferrum 协议远程后端(Opus / AES-256-GCM / 鉴权 / 模型选择)
- `stt_openai` (default): 标准 OpenAI 兼容远程后端

两个 feature 默认同时开启;实际使用哪个由运行时 `[stt] backend` 决定。
翻译**恒编译**,走非 optional 的 `reqwest`(DeepL 兼容 + LibreTranslate 双协议远程客户端),
运行时经 `[translate] backend` 选择;`translators`(内置 Google 翻译)已移除。

## License

按原项目 license

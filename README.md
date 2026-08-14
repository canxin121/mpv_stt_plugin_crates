# MPV STT Plugin Workspace

统一的 Rust workspace，管理 MPV 实时字幕插件的所有 crates。插件是**纯远程客户端**，
通过 `[stt] backend` 配置在运行时选择后端，不内置任何本地推理引擎。

## 架构

### Crates 结构

```
mpv_stt_plugin_crates/
├── crates/
│   ├── mpv-stt-common/      # 通用错误类型和工具
│   ├── mpv-stt-crypto/      # 加密和认证（ferrum 协议 AES-256-GCM / auth token）
│   ├── mpv-stt-srt/         # SRT 字幕文件处理
│   └── mpv-stt-plugin/      # MPV 插件主体（纯远程客户端）
└── Cargo.toml               # Workspace 配置
```

### 依赖关系

- **mpv-stt-common**: 最底层，无依赖
- **mpv-stt-crypto**: 依赖 common
- **mpv-stt-srt**: 依赖 common
- **mpv-stt-plugin**: 依赖 common, crypto, srt（以及按 feature 开启的 HTTP 后端依赖）

### 后端

插件支持两个远程后端，**同时编译、运行时选择**（`config.stt.backend`）：

| Cargo feature | 配置字段 | 协议 |
|---|---|---|
| `stt_ferrum` | `[stt.ferrum]` | 自定义 ferrum 协议：raw-body POST `/transcribe`，支持 Opus 压缩 / AES-256-GCM 加密 / 鉴权 / 模型选择（`x-model`）/ 语言提示（`x-language`） |
| `stt_openai` | `[stt.openai]` | 标准 OpenAI `POST /v1/audio/transcriptions`（multipart），任何兼容服务端可用 |

ferrum 协议的服务端由 [subtitle-gateway](https://github.com/canxin121/subtitle-gateway)
（FunASR ASR + 翻译统一网关）实现，同一端点复用同一套 FunASR 引擎。

## 编译

### 系统依赖

```bash
# Ubuntu/Debian
sudo apt-get install clang git

# Arch Linux
sudo pacman -S clang git

# macOS
brew install llvm git
```

**注意**: 不需要安装 `libmpv-dev`，构建脚本会自动下载 mpv 头文件。

### 快速开始

**一条命令完成所有准备**：

```bash
# Bash/Zsh 用户
source ./scripts/setup-deps.sh

# Fish 用户
source ./scripts/setup-deps.fish

# 完成后直接编译（下载 ~200MB MPV 头文件，首次约 1-2 分钟）
cargo build --release -p mpv-stt-plugin
```

**日常开发推荐使用 direnv 自动激活**：
```bash
# 首次设置后运行一次（所有 shell 通用）
direnv allow

# ✓ 之后进入目录自动激活，离开自动卸载
```

### 多平台构建

**一键构建所有平台和 feature 组合**：

```bash
# 1. 激活环境
source ./scripts/setup-deps.sh

# 2. (Android 构建必需) 配置 Android NDK
# export ANDROID_NDK_HOME=~/Android/Sdk/ndk/26.1.10909125

# 3. 运行统一构建脚本
./scripts/build-all.sh
```

**支持平台**：
- ✅ Linux x86_64 - 始终构建
- ⚠️ Android aarch64 - 需要 Android NDK
- ⚠️ Android armv7 - 需要 Android NDK

**产物位置**：
```
dist/
├── linux-x86_64/           # 始终生成
│   └── plugin/{ferrum,openai}.so
├── android-aarch64/        # 构建 Android 时生成
│   └── plugin/openai.so
└── android-armv7/...       # 构建 Android 时生成
```

**已知限制**：
- Android 交叉编译仅 `stt_openai`（`stt_ferrum` 依赖的 opusic-sys 未验证 Android 交叉编译）。

**Android 构建说明**：
- 脚本会自动拉取 mpv 官方仓库与 FFmpeg（默认 mpv master / FFmpeg n8.0），在本地交叉编译生成 `libmpv.so` 到 `target/android-prefix/<arch>/`。
- 必须提供 NDK (`ANDROID_NDK_HOME`)，脚本不再使用/下载 mpv-android。
- 详见 [scripts/README.md](./scripts/README.md#build-allsh) 了解脚本细节

### 安装

**插件**：
```bash
cp target/release/libmpv_stt_plugin.so ~/.config/mpv/scripts/
```

### 配置

插件通过 `mpv_stt_plugin_rs.toml` 配置（路径见 mpv 的 `--no-config` 之外的加载逻辑，
通常为 `~/.config/mpv/mpv_stt_plugin_rs.toml`）。选择后端：

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

### 翻译（远程）

翻译同样走**远程接口**——插件不再内置翻译逻辑（已移除对 Google 网页接口的直连），
而是调用 DeepL 兼容协议：`POST {server}/v1/translate`，鉴权头
`Authorization: DeepL-Auth-Key {key}`，body
`{"text": [...], "target_lang": "ZH", "source_lang": "EN"}`（source_lang 省略 = auto），
响应 `{"translations": [{"text": "..."}]}`。

```toml
[translate]
from_lang = "en"
to_lang = "zh"
concurrency = 4
server_addr = "http://127.0.0.1:8000"   # DeepL 兼容服务基址（subtitle-gateway 网关或任意上游）
api_key = ""                              # 可选：网关鉴权用 DeepL-Auth-Key
```

subtitle-gateway 提供该协议的服务端端点 `/v1/translate`，转发到配置的上游翻译服务
（官方 DeepL / 自建 DeepLX / 任意 DeepL 兼容）：
```bash
./run.sh --port 8000 \
  --translate-upstream https://api-free.deepl.com \
  --translate-upstream-key <上游key> \
  --translate-api-key <网关key>     # 可选：客户端须带此 key
```

### 翻译双协议（DeepL 兼容 + LibreTranslate）

插件同时编译两种翻译协议，`[translate] backend` 运行时选择（对称于 `[stt] backend`）：

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

### mpv-stt-plugin

- `stt_ferrum` (default): 自定义 ferrum 协议远程后端（Opus / AES-256-GCM / 鉴权 / 模型选择）
- `stt_openai` (default): 标准 OpenAI 兼容远程后端

两个 feature 默认同时开启；实际使用哪个由运行时 `[stt] backend` 决定。
翻译**恒编译**，走非 optional 的 `reqwest`（DeepL 兼容 + LibreTranslate 双协议远程客户端），
运行时经 `[translate] backend` 选择；`translators`（内置 Google 翻译）已移除。

## 从旧仓库迁移

原有项目：
- `/mnt/disk1/shared/git/mpv_stt_plugin_rs` → `crates/mpv-stt-plugin`

已拆分的共享代码：
- 错误类型 → `mpv-stt-common`
- 加密认证 → `mpv-stt-crypto`
- SRT 处理 → `mpv-stt-srt`

已移除（不再维护）：
- `mpv-stt-server`（Rust whisper.cpp 服务端，已由 subtitle-gateway 取代）
- `mpv-stt-protocol`（UDP 协议 crate）

## 优势

1. **统一依赖管理**: Workspace 级别的版本控制
2. **纯远程客户端**: 无内置推理引擎，插件保持轻量
3. **代码复用**: 共享 crates 可被多个项目使用
4. **清晰边界**: 每个 crate 职责单一
5. **更快编译**: 增量编译优化

## License

按原项目 license

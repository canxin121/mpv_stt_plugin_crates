# MPV STT Plugin Workspace

统一的 Rust workspace，管理 MPV 字幕转录插件及服务器的所有 crates。

## 架构

### Crates 结构

```
mpv_stt_plugin_crates/
├── crates/
│   ├── mpv-stt-common/      # 通用错误类型和工具
│   ├── mpv-stt-crypto/      # 加密和认证
│   ├── mpv-stt-protocol/    # UDP 通信协议
│   ├── mpv-stt-srt/         # SRT 字幕文件处理
│   ├── mpv-stt-plugin/      # MPV 插件主体
│   └── mpv-stt-server/      # UDP 推理服务器
└── Cargo.toml               # Workspace 配置
```

### 依赖关系

- **mpv-stt-common**: 最底层，无依赖
- **mpv-stt-crypto**: 依赖 common
- **mpv-stt-srt**: 依赖 common
- **mpv-stt-protocol**: 依赖 common, crypto
- **mpv-stt-plugin**: 依赖所有基础 crates
- **mpv-stt-server**: 依赖 protocol, crypto, plugin (default-features = false)

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
cargo build --release -p mpv-stt-server
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
│   ├── plugin/{cpu,cuda,remote}.so
│   └── server/{cpu,cuda}
├── android-aarch64/        # 构建 Android 时生成
│   ├── plugin/{cpu,remote}.so
│   └── server/cpu
└── android-armv7/...       # 构建 Android 时生成
```

**Android 构建说明**：
- 脚本会自动拉取 mpv 官方仓库与 FFmpeg（默认 mpv master / FFmpeg n8.0），在本地交叉编译生成 `libmpv.so` 到 `target/android-prefix/<arch>/`。
- 必须提供 NDK (`ANDROID_NDK_HOME`)，脚本不再使用/下载 mpv-android。
- 详见 [scripts/README.md](./scripts/README.md#build-allsh) 了解脚本细节

### 安装

**插件**：
```bash
cp target/release/libmpv_stt_plugin.so ~/.config/mpv/scripts/
```

**服务器**：
```bash
# 输出位置: target/release/mpv-stt-server
./crates/mpv-stt-server/run.sh  # 快速运行
```

## Features

### mpv-stt-plugin

- `stt_local_cpu` (default): 本地 CPU Whisper 推理
- `stt_local_cuda`: 本地 CUDA GPU 推理
- `stt_remote_tcp`: 远程 UDP 服务器推理

### mpv-stt-server

- `stt_local_cpu` (default): 使用 CPU Whisper
- `stt_local_cuda`: 使用 CUDA GPU

## 从旧仓库迁移

原有项目：
- `/mnt/disk1/shared/git/mpv_stt_server` → `crates/mpv-stt-server`
- `/mnt/disk1/shared/git/mpv_stt_plugin_rs` → `crates/mpv-stt-plugin`

已拆分的共享代码：
- 错误类型 → `mpv-stt-common`
- 加密认证 → `mpv-stt-crypto`
- UDP 协议 → `mpv-stt-protocol`
- SRT 处理 → `mpv-stt-srt`

## 优势

1. **统一依赖管理**: Workspace 级别的版本控制
2. **消除循环依赖**: Server 不再依赖完整的 plugin
3. **代码复用**: 共享 crates 可被多个项目使用
4. **清晰边界**: 每个 crate 职责单一
5. **更快编译**: 增量编译优化

## License

按原项目 license

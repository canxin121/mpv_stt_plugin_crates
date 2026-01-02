# Build Scripts

简洁的依赖管理脚本，直接在当前 shell 设置构建所需环境变量（不生成 `.envrc`）。

## 使用流程

### 一条命令完成所有准备

**Bash/Zsh 用户**：
```bash
source ./scripts/setup-deps.sh
```

**Fish 用户**：
```fish
source ./scripts/setup-deps.fish
```

这会自动：
1. 下载 MPV 头文件（如未下载）
2. 在当前 shell 设置构建环境变量
3. **激活环境到当前 shell**

之后直接使用 `cargo build`，无需额外操作。

**输出示例**：
```
==> Setting up MPV STT build dependencies
✓ MPV headers cloned to target/mpv-headers
✓ Found mpv/client.h at target/mpv-headers/libmpv
→ Exporting build environment variables to current shell...
✓ Environment variables exported

==> Setup complete!

Ready to build:
  cargo build --release -p mpv-stt-plugin
  cargo build --release -p mpv-stt-server
```

---

### 日常开发环境激活

每个新终端会话直接重新 source 对应脚本即可：

```bash
# Bash/Zsh
source ./scripts/setup-deps.sh

# Fish
source ./scripts/setup-deps.fish
```

---

### 使用 Cargo

```bash
# 环境激活后，像普通 Rust 项目一样
cargo build --release -p mpv-stt-plugin
cargo check --workspace
cargo test
cargo clippy
```

---

### 退出环境（可选）

**Bash/Zsh**：
```bash
unset MPV_INCLUDE_DIR BINDGEN_EXTRA_CLANG_ARGS CMAKE_INSTALL_LIBDIR
```

**Fish**：
```fish
set -e MPV_INCLUDE_DIR BINDGEN_EXTRA_CLANG_ARGS CMAKE_INSTALL_LIBDIR
```

---

## 完整工作流示例

### 首次使用

```bash
cd /mnt/disk1/shared/git/mpv_stt_plugin_crates

# 1. 一条命令完成设置和激活（约 1-2 分钟）
source ./scripts/setup-deps.sh        # Bash/Zsh
# 或
source ./scripts/setup-deps.fish      # Fish

# 2. 直接编译
cargo build --release -p mpv-stt-plugin
cargo build --release -p mpv-stt-server

# 3. 安装
cp target/release/libmpv_stt_plugin.so ~/.config/mpv/scripts/
```

### 日常开发

```bash
cd /mnt/disk1/shared/git/mpv_stt_plugin_crates

# 每个新终端会话激活环境
source ./scripts/setup-deps.sh   # Bash/Zsh
# 或
source ./scripts/setup-deps.fish # Fish

# 开发循环
cargo check
cargo test
cargo build --release
```

---

## 脚本详解

### `setup-deps.sh` / `setup-deps.fish`

**职责**：一次性依赖设置和环境激活

**步骤**：
1. 检查 `target/mpv-headers/` 是否存在
2. 不存在则 `git clone --depth 1` MPV 仓库
3. 查找 `mpv/client.h` 位置
4. 将构建所需环境变量导出到当前 shell（不写入文件）

**使用方式**：
```bash
# Bash/Zsh
source ./scripts/setup-deps.sh

# Fish
source ./scripts/setup-deps.fish
```

**注意**：脚本必须使用 `source` 调用，直接执行会报错。

**环境变量**：
- `MPV_REPO`: 自定义 MPV 仓库 URL（默认官方 GitHub）

**重新运行**：
```bash
# 重新下载 MPV 头文件
rm -rf target/mpv-headers
source ./scripts/setup-deps.sh  # 或 .fish
```

**幂等性**：多次运行安全，已存在的依赖不会重复下载。

---

### `build-all.sh`

**职责**：多平台、多 feature 统一构建脚本

**支持平台**：
- `linux-x86_64` - 本地 Linux x86_64
- `android-aarch64` - Android ARM64
- `android-armv7` - Android ARMv7

**支持 Features**：
- 插件：`stt_local_cpu`, `stt_local_cuda`, `stt_remote_http`
- 服务器：`stt_local_cpu`, `stt_local_cuda`

**使用方法**：

```bash
# 1. 激活环境（必须）
source ./scripts/setup-deps.sh   # 或 ./scripts/setup-deps.fish

# 2. （Android 必需）指定 NDK
# export ANDROID_NDK_HOME=~/Android/Sdk/ndk/26.1.10909125

# 3. 运行构建
./scripts/build-all.sh
```

**Android 依赖如何获取？**
- 脚本会自动拉取 **mpv 官方仓库** 与 **FFmpeg**（默认分支：mpv `master`，FFmpeg `n8.0`），并在本地交叉编译生成 `libmpv.so` 到 `target/android-prefix/<arch>/`.
- 需要你提供可用的 Android NDK（设置 `ANDROID_NDK_HOME`）。脚本不会再下载 mpv-android 或自带 NDK。
- 可覆盖的环境变量：
  - `ANDROID_PREFIX_BASE`：产出前缀根目录
  - `ANDROID_SRC_BASE`：源码缓存目录（mpv/ffmpeg）
  - `ANDROID_MPV_REPO` / `ANDROID_MPV_BRANCH`：mpv 源
  - `ANDROID_FFMPEG_REPO` / `ANDROID_FFMPEG_BRANCH`：FFmpeg 源

**产物结构**：

```
dist/
├── linux-x86_64/
│   ├── plugin/
│   │   ├── libmpv_stt_plugin_cpu.so
│   │   ├── libmpv_stt_plugin_cuda.so
│   │   └── libmpv_stt_plugin_remote.so
│   └── server/
│       ├── mpv-stt-server_cpu
│       └── mpv-stt-server_cuda
├── android-aarch64/
│   ├── plugin/
│   │   ├── libmpv_stt_plugin_cpu.so
│   │   └── libmpv_stt_plugin_remote.so
│   └── server/
│       └── mpv-stt-server_cpu
├── android-armv7/
│   └── ...
├── MANIFEST.txt          # 产物清单
└── build.log             # 构建日志
```

**环境变量**：
- `ANDROID_NDK_HOME`: 必需（构建 Android 时）；指向本地 NDK 根目录。
- `ANDROID_SRC_BASE`: Android 源码缓存目录（默认 `target/android-src`，包含 mpv/ffmpeg）。
- `ANDROID_PREFIX_BASE`: 生成的交叉前缀输出目录（默认 `target/android-prefix`）。
- `ANDROID_MPV_REPO` / `ANDROID_MPV_BRANCH`: mpv 源仓与分支。
- `ANDROID_FFMPEG_REPO` / `ANDROID_FFMPEG_BRANCH`: FFmpeg 源仓与分支/标签。

**注意事项**：
- Android 不支持 CUDA feature（自动跳过）
- 首次构建会自动安装所需的 Rust target（`rustup target add ...`）
- 构建失败的任务会记录到 `dist/build.log`
- 所有产物按平台和类型分类存放

---

## 环境变量说明

`setup-deps.sh` / `setup-deps.fish` 会在当前 shell 导出：

| 变量 | 用途 | 示例值 |
|------|------|--------|
| `MPV_INCLUDE_DIR` | mpv-client-sys 查找头文件路径 | `target/mpv-headers/libmpv` |
| `BINDGEN_EXTRA_CLANG_ARGS` | 传递给 bindgen 的参数 | `-I{MPV_INCLUDE_DIR}` |
| `CMAKE_INSTALL_LIBDIR` | libopusenc-static-sys 构建配置 | `lib` |

---

## 文件说明

```
mpv_stt_plugin_crates/
├── .cargo/config.toml     # Cargo 静态配置
├── dist/                  # 构建产物目录（build-all.sh 生成）
└── scripts/
    ├── setup-deps.sh      # 依赖设置脚本 (Bash/Zsh)
    ├── setup-deps.fish    # 依赖设置脚本 (Fish)
    ├── build-all.sh       # 多平台构建脚本
    └── README.md          # 本文档
```

**关系**：
- `.cargo/config.toml` - 静态配置（CMAKE_INSTALL_LIBDIR）
- `setup-deps.sh` / `setup-deps.fish` - 下载依赖并在当前 shell 导出环境变量
- `build-all.sh` - 依赖已导出的环境变量进行跨平台构建

---

## 疑难解答

### Q: 为什么必须用 `source`？

A:
- 环境变量必须设置到**当前 shell**，子进程无法修改父进程环境
- `source` 在当前 shell 执行脚本，环境变量直接生效
- 直接执行 `./scripts/setup-deps.sh` 会报错并提示正确用法

### Q: Fish 用户如何使用？

A:
```fish
source ./scripts/setup-deps.fish   # 每个新会话都需要
```

脚本会直接导出 `MPV_INCLUDE_DIR`、`BINDGEN_EXTRA_CLANG_ARGS`、`CMAKE_INSTALL_LIBDIR`，无需 `.envrc` 或额外转换。

### Q: 可以直接用系统的 libmpv-dev 吗？

A: 可以，在运行脚本前或代替脚本设置环境变量即可：
```bash
export MPV_INCLUDE_DIR="/usr/include"
export BINDGEN_EXTRA_CLANG_ARGS="-I${MPV_INCLUDE_DIR}"
export CMAKE_INSTALL_LIBDIR="${CMAKE_INSTALL_LIBDIR:-lib}"
```

### Q: 如何清理环境？

```bash
# 清理依赖
rm -rf target/mpv-headers
```

关闭终端即可移除导出的变量；或手动 `unset MPV_INCLUDE_DIR BINDGEN_EXTRA_CLANG_ARGS CMAKE_INSTALL_LIBDIR`（Fish: `set -e ...`）。

### Q: CI/CD 如何使用？

```bash
# GitHub Actions 示例
- name: Setup dependencies
  run: source ./scripts/setup-deps.sh

- name: Build
  run: cargo build --release --workspace
```

---

## 设计原则

1. **简洁性** - 一个脚本，直接导出所需环境变量
2. **幂等性** - 重复运行安全
3. **透明性** - 环境变量一目了然
4. **灵活性** - 支持 Bash/Zsh/Fish 及 CI

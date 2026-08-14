# Build Scripts

仓库根即单 crate;`scripts/build-all.sh` 负责跨平台构建,内含依赖引导
(mpv 头文件 clone / Android NDK 工具链设置),不再需要单独的 `setup-deps.*`。

## `build-all.sh` — 多平台构建

**职责**: 全平台(桌面 + Android)统一构建。每个平台/ABI 产出**一个**动态库
(默认把 `stt_ferrum` + `stt_openai` 两个后端同时编入,运行时由 `config.stt.backend`
切换);`-f` 可覆盖为单后端专用构建。

**支持平台**(每平台原生编译,CI 负责跨平台;桌面全部**动态链接** FFmpeg):

| 平台 | FFmpeg 来源 | Rust target | 产物 |
|---|---|---|---|
| `linux-x86_64` | BtbN 共享包(自动下载) | `x86_64-unknown-linux-gnu` | `libmpv_stt_plugin.so` |
| `darwin-arm64` | brew ffmpeg | `aarch64-apple-darwin` | `libmpv_stt_plugin.dylib` |
| `darwin-x86_64` | brew ffmpeg | `x86_64-apple-darwin` | `libmpv_stt_plugin.dylib` |
| `windows-x86_64` | BtbN 共享包(自动下载) | `x86_64-pc-windows-msvc` | `mpv_stt_plugin.dll` |
| `android` (默认 arm64-v8a) | Android 侧动态 libffmpeg(随 APK) | `aarch64-linux-android`(默认;32 位 ABI 受上游阻塞,见下) | `libmpv_stt_plugin.so` |

**动态 FFmpeg 解析**(`build_desktop` 内,无源码编译):
- macOS: 取 `FFPREFIX`(可覆盖),否则 `brew --prefix ffmpeg`,再退 `pkg-config`;
  插件直接链接 brew 的 dylib。
- Linux / Windows: 下载 BtbN/FFmpeg-Builds 的 **8.1 稳定别名**资产
  `releases/latest/download/ffmpeg-n8.1-latest-{linux64|win64}-lgpl-shared-8.1.{tar.xz|zip}`,
  解压缓存到 `target/ffmpeg-btbn/<platform>`,`FFMPEG_DIR` 指向缓存目录;
  `FFMPEG_BTBN_URL` 可覆盖该 URL。
  固定 8.1 而非 master 的原因:master 头已含 CUARRAY API(`CUarray`),ffmpeg-sys-next
  9.0.0 的 bindgen wrapper 未 stub 它,在无 CUDA SDK 的机器上会绑定失败。
- 三个平台都可通过环境变量 `FFMPEG_DIR` 直接指定(跳过自动解析)。
- Linux / Windows 的 FFmpeg 运行时库(`.so` / `.dll`)自动复制到
  `dist/<平台>/runtime/`,随插件一起分发。

**使用方法**:

```bash
# 1. 全平台矩阵(默认): 桌面 4 平台 + Android 默认 arm64-v8a
./scripts/build-all.sh

# 2. 指定平台(如当前 macOS 宿主)
./scripts/build-all.sh -p darwin-arm64

# 3. 单后端专用构建(默认双后端)
./scripts/build-all.sh -p darwin-arm64 -f stt_openai

# 4. Android(需 NDK;默认 arm64-v8a。32 位 ABI 受上游 ffmpeg-sys-next 的
#    Vulkan 假头 assert 阻塞:`sizeof(VkPhysicalDeviceFeatures2) == 240` 仅对
#    64 位指针成立,见 build-all.sh 内 DEFAULT_ANDROID_ABIS 注释)
export ANDROID_NDK_HOME=~/Library/Android/sdk/ndk/26.1.10909125
./scripts/build-all.sh -p android -a arm64-v8a

# 5. 仅检查不产物 / 清理 dist
./scripts/build-all.sh --check
./scripts/build-all.sh --clean
./scripts/build-all.sh -l   # 列出支持的平台/feature/ABI
```

**Android 依赖如何获取?**
- 脚本自动拉取 **mpv 官方仓库**与 **FFmpeg**,通过 `scripts/android-mpv` 交叉编译
  生成 `libmpv.so` 到 `target/android-mpv/prefix/<arch>/usr/local`。
- 需要提供 Android NDK(`ANDROID_NDK_HOME` / `NDK` / `CMAKE_ANDROID_NDK`)。
- 可覆盖的环境变量:
  - `ANDROID_PREFIX_BASE`: 交叉前缀根目录(默认 `target/android-mpv/prefix`)
  - `ANDROID_WORK_DIR`: 构建工作目录(默认 `target/android-mpv`)
  - `ANDROID_API`: Android API level(默认 21)

**产物结构**(单 .so,双后端编入):

```
dist/
├── linux-x86_64/plugin/libmpv_stt_plugin.so
│   └── runtime/lib*.so*                 # FFmpeg 运行时库
├── darwin-arm64/plugin/libmpv_stt_plugin.dylib
├── darwin-x86_64/plugin/libmpv_stt_plugin.dylib
├── windows-x86_64/plugin/mpv_stt_plugin.dll
│   └── runtime/*.dll                    # FFmpeg 运行时库
├── android/
│   └── arm64-v8a/plugin/libmpv_stt_plugin.so   # 默认 ABI
├── MANIFEST.txt          # 产物清单
└── build.log             # 构建日志
```

**环境变量**(首个平台构建时由脚本自动导出;可用来自定义):
- `MPV_INCLUDE_DIR`: mpv-client-sys 查找头文件路径(默认克隆到 `target/mpv-headers`)
- `BINDGEN_EXTRA_CLANG_ARGS`: 传给 bindgen 的参数(自动设为 mpv 头目录)
- `FFMPEG_DIR`: 直接指定 FFmpeg 前缀(桌面跳过自动解析)
- `FFPREFIX`: macOS 的 brew ffmpeg 前缀(默认 `brew --prefix ffmpeg`)
- `FFMPEG_BTBN_URL`: Linux/Windows 覆盖 BtbN 共享包下载 URL
- `ANDROID_NDK_HOME` / `NDK`: Android NDK 根目录(构建 Android 时必需)
- `ANDROID_PREFIX_BASE` / `ANDROID_WORK_DIR` / `ANDROID_API`: 见上

**注意事项**:
- 桌面平台用 `--target` 显式指定;本机交叉编译其它桌面 target 需自备工具链
  (脚本会打印警告)。**CI 各平台原生构建**,见 `.github/workflows/build.yml`。
- 首次构建会自动 `rustup target add ...`。
- 失败的构建记录在 `dist/build.log`。
- BtbN 共享包缓存于 `target/ffmpeg-btbn`,重复构建不重复下载。

## `android-mpv/` — Android 交叉编译辅助

`scripts/android-mpv/buildall.sh` 被 `build-all.sh` 在 Android 构建时自动调用,
用于交叉编译 mpv 及 FFmpeg 等依赖(需 meson/ninja/pkg-config/cmake)。

## CI

`.github/workflows/build.yml`:
- **desktop 矩阵**: `ubuntu-latest` → linux-x86_64、`macos-latest` → darwin-arm64、
  `windows-latest` → windows-x86_64。三平台全部动态链接 FFmpeg(不再源码编译),
  Windows 从 best-effort 转为正式平台;macOS runner 会 `brew install ffmpeg`。
- **android job**: `ubuntu-latest` 下载 NDK r29,构建 arm64-v8a(默认 ABI;
  32 位 ABI 暂受上游 ffmpeg-sys-next Vulkan-stub assert 阻塞)。
- 产物以 `dist-<platform>` / `dist-android` 上传为 GitHub Actions artifacts。

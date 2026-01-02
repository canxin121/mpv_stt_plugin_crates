从 workspace 根目录编译：
```bash
# 首次设置依赖
../../scripts/setup-deps.sh

# 每个新终端会话激活环境
source ../../scripts/setup-deps.sh

# 编译
cargo build --release -p mpv-stt-plugin
```

测试运行：
```bash
timeout 10s env MPV_STT_PLUGIN_RS_LOG=trace mpv ~/Downloads/video.mp4 &> log
```

插件输出：../../target/release/libmpv_stt_plugin.so (或 .dylib/.dll)
安装位置：/home/canxin/.config/mpv/scripts

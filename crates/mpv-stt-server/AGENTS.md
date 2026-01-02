从 workspace 根目录编译：
```bash
# 首次设置依赖
../../scripts/setup-deps.sh

# 每个新终端会话激活环境
source ../../scripts/setup-deps.sh

# 编译
cargo build --release -p mpv-stt-server
```

运行服务器：
```bash
./run.sh  # 从当前目录运行
# 或
../../target/release/mpv-stt-server --help
```

二进制输出：../../target/release/mpv-stt-server

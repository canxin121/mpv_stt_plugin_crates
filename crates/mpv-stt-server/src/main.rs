mod server;
mod worker;

use anyhow::Result;
use clap::Parser;
use log::info;
use mpv_stt_plugin::LocalModelConfig;

#[derive(Parser, Debug)]
#[command(author, version, about = "MPV STT HTTP Server", long_about = None)]
struct Args {
    /// HTTP bind address
    #[arg(short, long, default_value = "0.0.0.0:9000")]
    bind: String,

    /// Path to Whisper model file
    #[arg(short, long, default_value = "ggml-base.bin")]
    model_path: String,

    /// Number of CPU threads for inference
    #[arg(short, long, default_value_t = 8)]
    threads: u8,

    /// Language code (e.g., "en", "zh", "auto")
    #[arg(short, long, default_value = "auto")]
    language: String,

    /// GPU device ID (CUDA only)
    #[arg(long, default_value_t = 0)]
    gpu_device: i32,

    /// Enable flash attention (CUDA only)
    #[arg(long)]
    flash_attn: bool,

    /// Inference timeout in milliseconds
    #[arg(long, default_value_t = 120000)]
    timeout_ms: u64,

    /// Number of worker threads
    #[arg(short, long, default_value_t = 4)]
    workers: usize,

    /// Run a one-shot warmup inference at startup to load model into memory
    #[arg(long, default_value_t = true)]
    warmup: bool,

    /// Enable AES-GCM encryption
    #[arg(long)]
    enable_encryption: bool,

    /// Encryption passphrase (required if encryption enabled)
    #[arg(long, default_value = "")]
    encryption_key: String,

    /// Authorization secret (required for token validation)
    #[arg(long, default_value = "")]
    auth_secret: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    info!("MPV STT TCP Server starting");
    info!("  Bind address: {}", args.bind);
    info!("  Model: {}", args.model_path);
    info!("  Threads: {}", args.threads);
    info!("  Language: {}", args.language);
    info!("  Workers: {}", args.workers);
    info!(
        "  Encryption: {}",
        if args.enable_encryption {
            "enabled"
        } else {
            "disabled"
        }
    );
    info!(
        "  Auth: {}",
        if !args.auth_secret.is_empty() {
            "enabled"
        } else {
            "disabled"
        }
    );

    if args.enable_encryption && args.encryption_key.is_empty() {
        anyhow::bail!("--encryption-key is required when --enable-encryption is set");
    }

    let whisper_config = LocalModelConfig::new(args.model_path)
        .with_threads(args.threads)
        .with_language(args.language)
        .with_gpu_device(args.gpu_device)
        .with_flash_attn(args.flash_attn)
        .with_timeout_ms(args.timeout_ms);

    let server_config = server::ServerConfig {
        enable_encryption: args.enable_encryption,
        encryption_key: args.encryption_key,
        auth_secret: args.auth_secret,
        warmup: args.warmup,
    };

    let server =
        server::HttpServer::bind(&args.bind, whisper_config, args.workers, server_config).await?;

    info!("Server ready, processing HTTP requests...");
    server.run().await?;

    Ok(())
}

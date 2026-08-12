mod config;

use axum::{Router, routing::get};
use config::Config;
use std::net::SocketAddr;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志：把 tracing 事件输出到终端
    tracing_subscriber::fmt().init();

    let config = Config::from_env();
    info!(model = %config.model_path, data_dir = %config.data_dir, "配置加载完成");

    // 路由表：目前只有健康检查，后续阶段在这里挂 /tags 相关接口
    let app = Router::new().route("/health", get(health));

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("RAG-Service 启动于 http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// GET /health：存活检查
async fn health() -> &'static str {
    "ok"
}

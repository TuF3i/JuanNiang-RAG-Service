//! 程序入口：配置 → 存储加载 → 写者 → HTTP 服务。

use juan_niang_rag_service::api::{AppState, router};
use juan_niang_rag_service::config::Config;
use juan_niang_rag_service::embedding::Embedder;
use juan_niang_rag_service::store::TagStore;
use juan_niang_rag_service::writer::Writer;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    print_banner();
    tracing_subscriber::fmt().init();

    let config = Config::from_env();
    std::fs::create_dir_all(&config.data_dir)?;
    info!(?config, "配置加载完成");

    // 嵌入线程（后台加载模型 + 预热，不阻塞启动）
    let embedder = Embedder::new(&config.model_path, config.n_threads, config.n_ctx);

    // 存储：加载双快照，缺失/损坏自愈重建
    let store = TagStore::load(&config).map_err(|e| anyhow::anyhow!("存储加载失败: {e}"))?;
    info!(
        "存储就绪: {} tag, {} 块",
        store.tag_count(),
        store.chunk_count()
    );

    // 写者任务（独占 store，发布快照）
    let writer = Arc::new(Writer::start(store, embedder.clone(), config.clone()));
    let state = Arc::new(AppState::new(writer, embedder, config.lru_capacity));

    let app = router(state);
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("RAG-Service 启动于 http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// 启动 banner：亮淡蓝色（ANSI 94）。
/// stdout 不是终端（重定向 / Docker 日志）时输出纯文本，避免转义码污染日志。
fn print_banner() {
    let banner = include_str!("../banner.txt");
    if std::io::stdout().is_terminal() {
        println!("\x1b[94m{banner}\x1b[0m");
    } else {
        println!("{banner}");
    }
}

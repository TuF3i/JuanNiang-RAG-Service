//! 应用配置：全部来自环境变量，未设置时用默认值。
//! 这样部署时不用改代码，改环境变量即可。

#[derive(Debug, Clone)]
pub struct Config {
    /// bge-small-zh-v1.5 Q8_0 GGUF 模型文件路径
    pub model_path: String,
    /// 数据目录（index.tvim / tags.bin 的存放处，后续阶段使用）
    pub data_dir: String,
    /// HTTP 监听地址
    pub host: String,
    pub port: u16,
    /// llama.cpp 推理线程数（基准 1 扫描完，把最优值填进默认值）
    pub n_threads: i32,
    /// 上下文长度（bge-small-zh 上限 512）
    pub n_ctx: u32,
}

impl Config {
    /// 从环境变量构建配置
    pub fn from_env() -> Self {
        Self {
            model_path: env_or("RAG_MODEL_PATH", "models/bge-small-zh-v1.5-q8_0.gguf"),
            data_dir: env_or("RAG_DATA_DIR", "data"),
            host: env_or("RAG_HOST", "127.0.0.1"),
            port: env_or("RAG_PORT", "3000")
                .parse()
                .expect("RAG_PORT 必须是数字"),
            n_threads: env_or("RAG_N_THREADS", "4")
                .parse()
                .expect("RAG_N_THREADS 必须是数字"),
            n_ctx: env_or("RAG_N_CTX", "512")
                .parse()
                .expect("RAG_N_CTX 必须是数字"),
        }
    }
}

/// 读环境变量，没设置就返回默认值
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

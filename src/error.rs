//! 统一错误类型：各模块的错误都收编到这里，API 层统一转 HTTP 响应。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("嵌入服务错误: {0}")]
    Embed(#[from] crate::embedding::EmbedError),

    #[error("向量索引错误: {0}")]
    Vector(#[from] crate::vector_index::VectorError),

    #[error("存储错误: {0}")]
    Store(#[from] crate::store::StoreError),

    #[error("写者线程错误: {0}")]
    Writer(#[from] crate::writer::WriterError),

    #[error("参数错误: {0}")]
    BadRequest(String),

    #[error("内部错误: {0}")]
    Internal(String),
}

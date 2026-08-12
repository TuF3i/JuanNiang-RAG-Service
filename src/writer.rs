//! 写者任务（阶段 5）：查询与写入互不阻塞的核心。
//!
//! - 写者线程独占 `TagStore`，通过 mpsc 收命令
//! - 每批命令：分块 → 批量嵌入 → 应用 → 持久化 → COW 发布 → 应答
//! - 读者永远只读 `Arc<IndexSnapshot>`（瞬时读锁取引用，检索无锁）
//! - 批量命令只发布/持久化一次（摊销 COW 成本）
//!
//! 持久化先于发布：读者永远看不到未落盘的数据，崩溃最多丢最后一次应答前
//! 的写入（由调用方感知）。

use crate::chunker::{ChunkConfig, split};
use crate::config::Config;
use crate::embedding::Embedder;
use crate::error::ServiceError;
use crate::store::IndexSnapshot;
use crate::store::TagStore;
use std::sync::Arc;
use std::sync::RwLock;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum WriterError {
    #[error("写者线程未响应（可能已退出）")]
    Closed,
}

/// 单次写入的统计
#[derive(Debug, Clone)]
pub struct WriteStats {
    pub chunk_count: usize,
    pub truncated: bool,
}

/// 写命令：通过 mpsc 发给写者线程
enum WriteCmd {
    Upsert {
        tag: Uuid,
        text: String,
        reply: oneshot::Sender<Result<WriteStats, ServiceError>>,
    },
    BatchUpsert {
        items: Vec<(Uuid, String)>,
        reply: oneshot::Sender<Vec<Result<WriteStats, ServiceError>>>,
    },
    Delete {
        tag: Uuid,
        reply: oneshot::Sender<Result<(), ServiceError>>,
    },
}

/// 写者句柄：Clone 可分享，内部是 `Sender<WriteCmd>` + 最新快照
#[derive(Clone)]
pub struct Writer {
    tx: mpsc::Sender<WriteCmd>,
    snapshot: Arc<RwLock<Arc<IndexSnapshot>>>,
}

impl Writer {
    /// 启动写者任务
    pub fn start(store: TagStore, embedder: Embedder, config: Config) -> Self {
        let initial = store.snapshot().expect("初始快照失败");
        let snapshot = Arc::new(RwLock::new(Arc::new(initial)));
        let (tx, rx) = mpsc::channel(256);

        let task = WriterTask {
            store,
            embedder,
            config,
            rx,
            snapshot: snapshot.clone(),
        };
        tokio::spawn(async move { task.run().await });

        Self { tx, snapshot }
    }

    /// 读者取最新快照：瞬时读锁拿 Arc，之后检索无锁
    pub fn snapshot(&self) -> Arc<IndexSnapshot> {
        self.snapshot.read().expect("快照锁中毒").clone()
    }

    /// 单条 upsert（读己之写：应答时数据已发布并落盘）
    pub async fn upsert(&self, tag: Uuid, text: String) -> Result<WriteStats, ServiceError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(WriteCmd::Upsert { tag, text, reply })
            .await
            .map_err(|_| ServiceError::Writer(WriterError::Closed))?;
        rx.await
            .map_err(|_| ServiceError::Writer(WriterError::Closed))?
    }

    /// 批量 upsert：一次嵌入 + 一次发布 + 一次持久化
    pub async fn batch_upsert(
        &self,
        items: Vec<(Uuid, String)>,
    ) -> Result<Vec<Result<WriteStats, ServiceError>>, ServiceError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(WriteCmd::BatchUpsert { items, reply })
            .await
            .map_err(|_| ServiceError::Writer(WriterError::Closed))?;
        let results = rx
            .await
            .map_err(|_| ServiceError::Writer(WriterError::Closed))?;
        Ok(results)
    }

    pub async fn delete(&self, tag: Uuid) -> Result<(), ServiceError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(WriteCmd::Delete { tag, reply })
            .await
            .map_err(|_| ServiceError::Writer(WriterError::Closed))?;
        rx.await
            .map_err(|_| ServiceError::Writer(WriterError::Closed))?
    }

    pub fn health(&self) -> (usize, usize) {
        let snap = self.snapshot();
        (snap.tag_count(), snap.chunk_count())
    }
}

/// 写者任务本体（独占 TagStore）
struct WriterTask {
    store: TagStore,
    embedder: Embedder,
    config: Config,
    rx: mpsc::Receiver<WriteCmd>,
    snapshot: Arc<RwLock<Arc<IndexSnapshot>>>,
}

impl WriterTask {
    async fn run(mut self) {
        while let Some(cmd) = self.rx.recv().await {
            // 顺手把队列里已积压的命令一起取出（批量合并）
            let mut cmds = vec![cmd];
            while let Ok(next) = self.rx.try_recv() {
                cmds.push(next);
            }
            for c in cmds {
                self.handle(c).await;
            }
        }
        info!("写者任务退出");
    }

    async fn handle(&mut self, cmd: WriteCmd) {
        match cmd {
            WriteCmd::Upsert { tag, text, reply } => {
                let result = self.apply_upsert(tag, text).await;
                let _ = reply.send(result);
            }
            WriteCmd::BatchUpsert { items, reply } => {
                // 整批只发布/持久化一次
                let results = self.apply_batch_upsert(items).await;
                let _ = reply.send(results);
            }
            WriteCmd::Delete { tag, reply } => {
                let result = self.apply_delete(tag);
                let _ = reply.send(result);
            }
        }
    }

    /// 单条 upsert：应用 → 持久化 → 发布
    async fn apply_upsert(&mut self, tag: Uuid, text: String) -> Result<WriteStats, ServiceError> {
        let (chunks, vectors, truncated) = self.prepare_chunks(std::slice::from_ref(&text)).await?;
        let chunks = chunks.into_iter().next().unwrap_or_default();
        let vectors = vectors.into_iter().next().unwrap_or_default();
        let truncated = truncated.into_iter().next().unwrap_or(false);
        if vectors.is_empty() {
            return Err(ServiceError::BadRequest("文本为空或分块后无内容".into()));
        }
        let n = self.store.upsert(tag, &chunks, &vectors)?;
        self.commit()?;
        debug!(tag = %tag, chunks = n, "upsert 完成");
        Ok(WriteStats {
            chunk_count: n,
            truncated,
        })
    }

    /// 批量 upsert：所有条目合并成一次嵌入，整批一次持久化+发布
    async fn apply_batch_upsert(
        &mut self,
        items: Vec<(Uuid, String)>,
    ) -> Vec<Result<WriteStats, ServiceError>> {
        let mut results = Vec::with_capacity(items.len());
        if items.is_empty() {
            return results;
        }

        // 1. 全部文本合并嵌入（一次批量推理）
        let texts: Vec<String> = items.iter().map(|(_, t)| t.clone()).collect();
        let prepared = self.prepare_chunks(&texts).await;
        let mut any_error = false;

        match prepared {
            Ok((all_chunks, all_vectors, all_truncated)) => {
                // 2. 逐条应用
                for (idx, (tag, _)) in items.iter().enumerate() {
                    let chunks = all_chunks[idx].clone();
                    let vectors = all_vectors[idx].clone();
                    let truncated = all_truncated[idx];
                    if vectors.is_empty() {
                        results.push(Err(ServiceError::BadRequest(
                            "文本为空或分块后无内容".into(),
                        )));
                        any_error = true;
                        continue;
                    }
                    match self.store.upsert(*tag, &chunks, &vectors) {
                        Ok(n) => results.push(Ok(WriteStats {
                            chunk_count: n,
                            truncated,
                        })),
                        Err(e) => {
                            results.push(Err(ServiceError::Store(e)));
                            any_error = true;
                        }
                    }
                }
            }
            Err(e) => {
                for _ in &items {
                    results.push(Err(ServiceError::Internal(e.to_string())));
                }
                any_error = true;
            }
        }

        // 3. 即使有条目失败，已成功的部分也要持久化+发布
        if (!any_error || results.iter().any(|r| r.is_ok()))
            && let Err(e) = self.commit()
        {
            warn!("批量持久化失败: {e}");
        }
        info!(n = items.len(), "批量 upsert 完成");
        results
    }

    fn apply_delete(&mut self, tag: Uuid) -> Result<(), ServiceError> {
        self.store.delete(tag)?;
        self.commit()?;
        info!(tag = %tag, "删除完成");
        Ok(())
    }

    /// 分块 + 批量嵌入。返回 (每文本的块列表, 每文本的向量列表, 每文本是否有截断)
    async fn prepare_chunks(
        &mut self,
        texts: &[String],
    ) -> Result<(Vec<Vec<String>>, Vec<Vec<Vec<f32>>>, Vec<bool>), ServiceError> {
        let cfg = ChunkConfig::new(self.config.max_chunk_chars, self.config.overlap_chars);
        let all_chunks: Vec<Vec<String>> = texts.iter().map(|t| split(t, &cfg)).collect();
        let flat: Vec<String> = all_chunks.iter().flatten().cloned().collect();
        if flat.is_empty() {
            return Err(ServiceError::BadRequest("文本为空".into()));
        }

        let (vectors, truncated_flags) = self.embedder.embed(flat, false).await?;

        // 按文本还原向量分组
        let mut all_vectors = Vec::with_capacity(texts.len());
        let mut all_truncated = Vec::with_capacity(texts.len());
        let mut cursor = 0;
        for chunks in &all_chunks {
            let n = chunks.len();
            all_vectors.push(vectors[cursor..cursor + n].to_vec());
            let any_truncated = truncated_flags[cursor..cursor + n].iter().any(|&b| b);
            all_truncated.push(any_truncated);
            cursor += n;
        }
        Ok((all_chunks, all_vectors, all_truncated))
    }

    /// 持久化 → COW 发布 → 替换读者快照
    fn commit(&mut self) -> Result<(), ServiceError> {
        self.store.persist()?;
        let snap = self.store.snapshot()?;
        snap.index.prepare();
        *self.snapshot.write().expect("快照锁中毒") = Arc::new(snap);
        Ok(())
    }
}

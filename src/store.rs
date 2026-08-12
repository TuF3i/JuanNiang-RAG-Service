//! 存储层（阶段 4）：tag（uuid）↔ 块向量（1:N）的持久化与状态管理。
//!
//! 双文件快照（零 SQL）：
//! - `index.tvim`：turbovec 压缩向量 + u64 块 id 映射（真源①）
//! - `tags.bin`：tag_to_ids + next_id + 原始归一化向量（真源②，重建用）
//!
//! 原子写：临时文件 + rename。崩溃最多丢最后一次发布。
//! 自愈：启动时 `index.len() != 块数总和` 则用原始向量重建索引。

use crate::config::Config;
use crate::vector_index::VectorError;
use crate::vector_index::VectorIndex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("序列化失败: {0}")]
    Serialize(String),
    #[error("反序列化失败: {0}")]
    Deserialize(String),
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("向量索引错误: {0}")]
    Vector(#[from] VectorError),
    #[error("tag 不存在: {0}")]
    TagNotFound(Uuid),
}

/// 发布给读者的不可变快照（写者 COW 拷贝后原子替换）
pub struct IndexSnapshot {
    pub index: VectorIndex,
    /// tag → 块 id 列表
    pub tag_to_ids: HashMap<Uuid, Vec<u64>>,
    /// 块 id → tag（聚合检索用）
    pub chunk_owner: HashMap<u64, Uuid>,
    pub next_id: u64,
}

impl IndexSnapshot {
    pub fn tag_count(&self) -> usize {
        self.tag_to_ids.len()
    }

    pub fn chunk_count(&self) -> usize {
        self.index.len()
    }
}

/// tags.bin 的磁盘格式
#[derive(Serialize, Deserialize, Default)]
struct TagsFile {
    tag_to_ids: HashMap<Uuid, Vec<u64>>,
    next_id: u64,
    /// 块 id → 原始归一化向量（重建索引用；可配置关闭）
    raw_vectors: HashMap<u64, Vec<f32>>,
}

/// 写者独占的可变状态
pub struct TagStore {
    index: VectorIndex,
    tag_to_ids: HashMap<Uuid, Vec<u64>>,
    next_id: u64,
    raw_vectors: HashMap<u64, Vec<f32>>,
    /// 是否持久化原始向量（配置）
    keep_raw: bool,
    data_dir: PathBuf,
}

impl TagStore {
    /// 从磁盘加载；无数据则返回空库
    pub fn load(config: &Config) -> Result<Self, StoreError> {
        let data_dir = PathBuf::from(&config.data_dir);
        let tags_path = data_dir.join("tags.bin");
        let index_path = data_dir.join("index.tvim");

        let mut store = if tags_path.exists() {
            Self::load_tags(&tags_path, &index_path, config)?
        } else {
            Self::empty(config)?
        };
        // 自愈：块数不一致 → 从原始向量重建索引
        let expected: usize = store.tag_to_ids.values().map(|v| v.len()).sum();
        if store.index.len() != expected {
            info!(
                "索引块数 {} 与 tags.bin 块数 {} 不一致，触发重建",
                store.index.len(),
                expected
            );
            store.rebuild_index()?;
            store.persist()?;
        }
        Ok(store)
    }

    fn load_tags(tags_path: &Path, index_path: &Path, config: &Config) -> Result<Self, StoreError> {
        let bytes = std::fs::read(tags_path).map_err(|e| StoreError::Io(e.to_string()))?;
        let f: TagsFile =
            bincode::deserialize(&bytes).map_err(|e| StoreError::Deserialize(e.to_string()))?;

        let index = if index_path.exists() {
            VectorIndex::load(index_path).unwrap_or_else(|e| {
                info!("index.tvim 加载失败（{e}），稍后由一致性校验触发重建");
                VectorIndex::new(config.dim, config.bit_width).expect("构造空索引失败")
            })
        } else {
            VectorIndex::new(config.dim, config.bit_width).expect("构造空索引失败")
        };

        Ok(Self {
            index,
            tag_to_ids: f.tag_to_ids,
            next_id: f.next_id,
            raw_vectors: f.raw_vectors,
            keep_raw: config.store_raw_vectors,
            data_dir: PathBuf::from(&config.data_dir),
        })
    }

    fn empty(config: &Config) -> Result<Self, StoreError> {
        Ok(Self {
            index: VectorIndex::new(config.dim, config.bit_width).map_err(StoreError::Vector)?,
            tag_to_ids: HashMap::new(),
            next_id: 0,
            raw_vectors: HashMap::new(),
            keep_raw: config.store_raw_vectors,
            data_dir: PathBuf::from(&config.data_dir),
        })
    }

    /// 从原始向量全量重建索引
    fn rebuild_index(&mut self) -> Result<(), StoreError> {
        let mut new_index = VectorIndex::new(self.index.dim(), 4).map_err(StoreError::Vector)?;
        // 原始向量缺失（keep_raw=false）时无法重建，保留原索引并告警
        if self.raw_vectors.is_empty() && !self.tag_to_ids.is_empty() {
            return Err(StoreError::Io(
                "原始向量未持久化，无法重建索引（需 Agent 重推全文）".to_string(),
            ));
        }
        let mut vectors = Vec::with_capacity(self.raw_vectors.len() * self.index.dim());
        let mut ids = Vec::with_capacity(self.raw_vectors.len());
        for (&id, v) in &self.raw_vectors {
            vectors.extend_from_slice(v);
            ids.push(id);
        }
        new_index
            .add_with_ids(&vectors, &ids)
            .map_err(StoreError::Vector)?;
        new_index.prepare();
        self.index = new_index;
        Ok(())
    }

    /// 原子双快照持久化：临时文件 + rename
    pub fn persist(&self) -> Result<(), StoreError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|e| StoreError::Io(e.to_string()))?;

        let tags_path = self.data_dir.join("tags.bin");
        let index_path = self.data_dir.join("index.tvim");
        let tags_tmp = self.data_dir.join("tags.bin.tmp");
        let index_tmp = self.data_dir.join("index.tvim.tmp");

        let f = TagsFile {
            tag_to_ids: self.tag_to_ids.clone(),
            next_id: self.next_id,
            raw_vectors: if self.keep_raw {
                self.raw_vectors.clone()
            } else {
                HashMap::new()
            },
        };
        let bytes = bincode::serialize(&f).map_err(|e| StoreError::Serialize(e.to_string()))?;
        std::fs::write(&tags_tmp, bytes).map_err(|e| StoreError::Io(e.to_string()))?;
        self.index.write(&index_tmp)?;

        // rename 是原子操作；先 tags 后 index，崩溃时以 tags.bin 为准做一致性校验
        std::fs::rename(&tags_tmp, &tags_path).map_err(|e| StoreError::Io(e.to_string()))?;
        std::fs::rename(&index_tmp, &index_path).map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }

    // ---------- 写操作（仅写者线程调用） ----------

    /// upsert：替换 tag 的全部旧块，写入新块
    pub fn upsert(
        &mut self,
        tag: Uuid,
        chunks: &[String],
        vectors: &[Vec<f32>],
    ) -> Result<usize, StoreError> {
        debug_assert_eq!(chunks.len(), vectors.len());
        let dim = self.index.dim();

        // 删除旧块
        if let Some(old_ids) = self.tag_to_ids.remove(&tag) {
            for id in old_ids {
                self.index.remove(id);
                self.raw_vectors.remove(&id);
            }
        }

        // 分配新块 id 并写入
        let mut ids = Vec::with_capacity(vectors.len());
        let mut flat = Vec::with_capacity(vectors.len() * dim);
        for v in vectors {
            if v.len() != dim {
                return Err(StoreError::Io(format!(
                    "向量维度 {}(实际) != {}(期望)",
                    v.len(),
                    dim
                )));
            }
            let id = self.next_id;
            self.next_id += 1;
            ids.push(id);
            flat.extend_from_slice(v);
            self.raw_vectors.insert(id, v.clone());
        }
        self.index.add_with_ids(&flat, &ids)?;
        self.tag_to_ids.insert(tag, ids);
        Ok(vectors.len())
    }

    /// 删除 tag 及其全部块
    pub fn delete(&mut self, tag: Uuid) -> Result<(), StoreError> {
        let ids = self
            .tag_to_ids
            .remove(&tag)
            .ok_or(StoreError::TagNotFound(tag))?;
        for id in ids {
            self.index.remove(id);
            self.raw_vectors.remove(&id);
        }
        Ok(())
    }

    // ---------- 快照发布 ----------

    /// COW 快照：索引走序列化往返拷贝，映射克隆
    pub fn snapshot(&self) -> Result<IndexSnapshot, StoreError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|e| StoreError::Io(e.to_string()))?;
        let tmp = self.data_dir.join(".cow.tvim");
        let index = self.index.copy(&tmp).map_err(StoreError::Vector)?;
        let _ = std::fs::remove_file(&tmp);

        let mut chunk_owner = HashMap::with_capacity(self.index.len());
        for (&tag, ids) in &self.tag_to_ids {
            for &id in ids {
                chunk_owner.insert(id, tag);
            }
        }
        Ok(IndexSnapshot {
            index,
            tag_to_ids: self.tag_to_ids.clone(),
            chunk_owner,
            next_id: self.next_id,
        })
    }

    pub fn tag_count(&self) -> usize {
        self.tag_to_ids.len()
    }

    pub fn chunk_count(&self) -> usize {
        self.index.len()
    }
}

// ---------- 单元测试（合成数据，不需要模型） ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(dir: &Path) -> Config {
        Config {
            model_path: "none".into(),
            data_dir: dir.to_string_lossy().into_owned(),
            host: "127.0.0.1".into(),
            port: 0,
            n_threads: 4,
            n_ctx: 512,
            dim: 8, // 测试用小维度（8 的倍数）
            bit_width: 4,
            max_chunk_chars: 260,
            overlap_chars: 50,
            store_raw_vectors: true,
            lru_capacity: 128,
        }
    }

    fn fake_vector(dim: usize, seed: f32) -> Vec<f32> {
        let mut v = vec![seed; dim];
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }

    #[test]
    fn upsert_delete_snapshot_cycle() {
        let dir = std::env::temp_dir().join(format!("rag_test_{}", Uuid::new_v4()));
        let cfg = test_config(&dir);
        let mut store = TagStore::empty(&cfg).unwrap();
        let tag = Uuid::new_v4();

        let n = store
            .upsert(
                tag,
                &["块一".into(), "块二".into()],
                &[fake_vector(8, 0.5), fake_vector(8, 0.7)],
            )
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.tag_count(), 1);
        assert_eq!(store.chunk_count(), 2);

        // 覆写：同 tag 再写 1 块，旧 2 块应消失
        let n = store
            .upsert(tag, &["新块".into()], &[fake_vector(8, 0.9)])
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.chunk_count(), 1);

        // 快照
        let snap = store.snapshot().unwrap();
        assert_eq!(snap.tag_count(), 1);
        assert_eq!(snap.chunk_count(), 1);
        assert_eq!(snap.chunk_owner.len(), 1);

        // 持久化 + 重新加载
        store.persist().unwrap();
        drop(store);
        let mut reloaded = TagStore::load(&cfg).unwrap();
        assert_eq!(reloaded.tag_count(), 1);
        assert_eq!(reloaded.chunk_count(), 1);
        assert_eq!(reloaded.next_id, 3, "两次 upsert 共分配了 3 个 id");

        // 删除
        reloaded.delete(tag).unwrap();
        assert_eq!(reloaded.tag_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_data_returns_empty() {
        let dir = std::env::temp_dir().join(format!("rag_missing_{}", Uuid::new_v4()));
        let cfg = test_config(&dir);
        let store = TagStore::load(&cfg).unwrap();
        assert_eq!(store.tag_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebuild_when_index_missing() {
        let dir = std::env::temp_dir().join(format!("rag_rebuild_{}", Uuid::new_v4()));
        let cfg = test_config(&dir);
        let mut store = TagStore::empty(&cfg).unwrap();
        store
            .upsert(Uuid::new_v4(), &["内容".into()], &[fake_vector(8, 0.3)])
            .unwrap();
        store.persist().unwrap();

        // 删掉 index.tvim，模拟损坏
        std::fs::remove_file(dir.join("index.tvim")).unwrap();

        // load 应触发自愈重建
        let reloaded = TagStore::load(&cfg).unwrap();
        assert_eq!(reloaded.chunk_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

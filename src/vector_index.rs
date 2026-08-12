//! 向量索引封装（阶段 3）：对 turbovec `IdMapIndex` 的薄封装。
//!
//! - 外部 id 是 u64（与 tag 的 uuid 映射由 store 层维护）
//! - `search` 返回 `(id, distance)`，distance 越小越相似
//! - COW 拷贝：`IdMapIndex` 无 `Clone`，用 `write → load` 序列化往返实现

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VectorError {
    #[error("构造索引失败: {0}")]
    Construct(String),
    #[error("添加向量失败: {0}")]
    Add(String),
    #[error("序列化失败: {0}")]
    Io(String),
    #[error("向量维度不符: 期望 {expected}, 实际 {actual}")]
    DimMismatch { expected: usize, actual: usize },
    #[error("输入向量为空")]
    Empty,
}

pub struct VectorIndex {
    index: turbovec::IdMapIndex,
    dim: usize,
}

impl VectorIndex {
    pub fn new(dim: usize, bit_width: usize) -> Result<Self, VectorError> {
        let index = turbovec::IdMapIndex::new(dim, bit_width)
            .map_err(|e| VectorError::Construct(e.to_string()))?;
        Ok(Self { index, dim })
    }

    /// 从 .tvim 文件加载
    pub fn load(path: impl AsRef<Path>) -> Result<Self, VectorError> {
        let index = turbovec::IdMapIndex::load(path.as_ref())
            .map_err(|e| VectorError::Io(e.to_string()))?;
        let dim = index.dim();
        Ok(Self { index, dim })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn contains(&self, id: u64) -> bool {
        self.index.contains(id)
    }

    /// 批量添加：vectors 是扁平的 n×dim f32
    pub fn add_with_ids(&mut self, vectors: &[f32], ids: &[u64]) -> Result<(), VectorError> {
        if vectors.is_empty() {
            return Err(VectorError::Empty);
        }
        if !vectors.len().is_multiple_of(self.dim) {
            return Err(VectorError::DimMismatch {
                expected: self.dim,
                actual: vectors.len(),
            });
        }
        self.index
            .add_with_ids(vectors, ids)
            .map_err(|e| VectorError::Add(e.to_string()))
    }

    /// 删除：O(1) swap_remove，外部 id 由 IdMapIndex 内部映射保持稳定
    pub fn remove(&mut self, id: u64) -> bool {
        self.index.remove(id)
    }

    /// 检索 top-k，返回 (id, score)，按 score 降序（turbovec 的 score 是相似度，越大越相似）
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)> {
        let (scores, ids) = self.index.search(query, k);
        let mut pairs: Vec<(u64, f32)> = ids.into_iter().zip(scores).collect();
        pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        pairs
    }

    /// 预热检索缓存（旋转矩阵/码本/SIMD 布局），启动或批量写入后调用
    pub fn prepare(&self) {
        self.index.prepare();
    }

    /// 写 .tvim 文件
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), VectorError> {
        self.index
            .write(path.as_ref())
            .map_err(|e| VectorError::Io(e.to_string()))
    }

    /// COW 拷贝：序列化往返得到一份独立的新索引（用于发布快照）
    pub fn copy(&self, tmp_path: impl AsRef<Path>) -> Result<Self, VectorError> {
        self.write(tmp_path.as_ref())?;
        Self::load(tmp_path.as_ref())
    }
}

// ---------- 单元测试（合成数据，不需要模型） ----------

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    const DIM: usize = 512;
    const BIT_WIDTH: usize = 4;

    fn unit_vector(rng: &mut impl Rng) -> Vec<f32> {
        let mut v: Vec<f32> = (0..DIM).map(|_| rng.gen_range(-1.0..1.0)).collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }

    fn make_index(n: usize) -> VectorIndex {
        let mut idx = VectorIndex::new(DIM, BIT_WIDTH).unwrap();
        let mut rng = rand::thread_rng();
        let mut vectors = Vec::with_capacity(n * DIM);
        let ids: Vec<u64> = (0..n as u64).collect();
        for _ in 0..n {
            vectors.extend(unit_vector(&mut rng));
        }
        idx.add_with_ids(&vectors, &ids).unwrap();
        idx.prepare();
        idx
    }

    #[test]
    fn search_returns_nearest() {
        let mut idx = VectorIndex::new(DIM, BIT_WIDTH).unwrap();
        // 两个正交向量：id 0 = e0, id 1 = e1
        let mut v0 = vec![0.0f32; DIM];
        v0[0] = 1.0;
        let mut v1 = vec![0.0f32; DIM];
        v1[1] = 1.0;
        let mut all = v0.clone();
        all.extend_from_slice(&v1);
        idx.add_with_ids(&all, &[0, 1]).unwrap();
        idx.prepare();

        let hits = idx.search(&v0, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, 0, "应该先命中 id 0");
        assert_eq!(hits[1].0, 1);
    }

    #[test]
    fn remove_keeps_other_ids_stable() {
        let mut idx = make_index(10);
        assert!(idx.remove(3));
        assert!(!idx.contains(3));
        // 其余 id 仍然可查
        for id in [0u64, 1, 2, 4, 9] {
            assert!(idx.contains(id), "id {id} 不应受影响");
        }
    }

    #[test]
    fn copy_roundtrip_equals_original() {
        let idx = make_index(1_000);
        let tmp = std::env::temp_dir().join("tvim_copy_test.tvim");
        let cloned = idx.copy(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(cloned.len(), idx.len());
        assert_eq!(cloned.dim(), idx.dim());
        // 同一查询，两侧 top-1 应一致
        let mut rng = rand::thread_rng();
        let q = unit_vector(&mut rng);
        let a = idx.search(&q, 3);
        let b = cloned.search(&q, 3);
        assert_eq!(a[0].0, b[0].0);
    }

    #[test]
    fn rejects_wrong_dim() {
        let mut idx = VectorIndex::new(DIM, BIT_WIDTH).unwrap();
        let err = idx.add_with_ids(&[1.0, 2.0], &[0]);
        assert!(err.is_err());
    }
}

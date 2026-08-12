//! 检索流水线（阶段 6）：向量 top-k → 按 tag 聚合去重 → 排序过滤。
//!
//! turbovec 返回的 score 是相似度（≈ 余弦，[-1, 1]，越大越相似），
//! 统一映射到 [0, 1]：`score = (raw + 1) / 2`，越高越好。

use serde::Serialize;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub tag: Uuid,
    pub score: f32,
}

/// 相似度 [-1, 1] → [0, 1]
fn to_score(raw: f32) -> f32 {
    ((raw + 1.0) / 2.0).clamp(0.0, 1.0)
}

/// 块级命中按 tag 聚合：每 tag 取最高分 → 过滤 → 排序 → 截取 top-k
pub fn aggregate(
    chunk_hits: &[(u64, f32)],
    chunk_owner: &HashMap<u64, Uuid>,
    k: usize,
    min_score: Option<f32>,
) -> Vec<SearchHit> {
    let mut best: HashMap<Uuid, f32> = HashMap::new();
    for (id, raw) in chunk_hits {
        if let Some(&tag) = chunk_owner.get(id) {
            let score = to_score(*raw);
            let e = best.entry(tag).or_insert(f32::MIN);
            if score > *e {
                *e = score;
            }
        }
    }

    let mut hits: Vec<SearchHit> = best
        .into_iter()
        .filter(|(_, score)| min_score.is_none_or(|m| *score >= m))
        .map(|(tag, score)| SearchHit { tag, score })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(k);
    hits
}

// ---------- 单元测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(pairs: &[(u64, Uuid)]) -> HashMap<u64, Uuid> {
        pairs.iter().cloned().collect()
    }

    #[test]
    fn groups_chunks_by_tag_taking_max() {
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        let owner = owner(&[(1, t1), (2, t1), (3, t2)]);
        // 相似度越高越好：t1 两块 0.9 和 0.6；t2 一块 0.7
        let hits = aggregate(&[(1, 0.8), (2, 0.2), (3, 0.4)], &owner, 10, None);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].tag, t1, "t1 应取最高分");
        assert!((hits[0].score - to_score(0.8)).abs() < 1e-6);
        assert_eq!(hits[1].tag, t2);
    }

    #[test]
    fn respects_k_and_min_score() {
        let t = Uuid::new_v4();
        let owner = owner(&[(1, t)]);
        let hits = aggregate(&[(1, 0.1)], &owner, 10, Some(0.8));
        assert!(
            hits.is_empty(),
            "相似度 {} 应被 min_score 过滤",
            to_score(0.1)
        );
    }

    #[test]
    fn unknown_chunk_skipped() {
        let t = Uuid::new_v4();
        let owner = owner(&[(1, t)]);
        let hits = aggregate(&[(1, 0.2), (99, 0.1)], &owner, 10, None);
        assert_eq!(hits.len(), 1);
    }
}

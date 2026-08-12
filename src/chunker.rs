//! 服务端内部分块（阶段 2）。
//!
//! 契约：Agent 端文本永不拆分，分块只发生在服务端内部，对外仍是 tag ↔ 全文。
//! 短文本（≤ max_chars）直接返回单块；长文本按句子边界切块并带重叠。
//!
//! 为什么不用 text-splitter：中文 BERT 分词 ≈ 1 字 1 token，用字符数近似
//! token 数即可（260 字符 ≈ 256 token），避免引入 tiktoken 依赖及其与
//! BERT 分词不一致的问题。全部为纯函数，可独立测试。

use serde::{Deserialize, Serialize};

/// 分块参数
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ChunkConfig {
    /// 单块最大字符数（≈ token 数）
    pub max_chars: usize,
    /// 相邻块重叠字符数
    pub overlap_chars: usize,
}

impl ChunkConfig {
    /// 默认：260 字符 ≈ 256 token，重叠 50 字符 ≈ 50 token
    pub fn new(max_chars: usize, overlap_chars: usize) -> Self {
        Self {
            max_chars,
            overlap_chars,
        }
    }
}

/// 句子分隔符（中文标点 + 换行），切分时保留在句尾
const SENTENCE_BOUNDARIES: &[char] = &['。', '！', '？', '；', '\n'];

/// 把文本切成块。短文本不切，直接单块。
///
/// 算法：固定字符窗口（max_chars），切点优先回退到最近的句子边界；
/// 下一块从 `end - overlap_chars` 开始，重叠精确可控，且保证推进。
pub fn split(text: &str, cfg: &ChunkConfig) -> Vec<String> {
    let total = text.chars().count();
    if total == 0 {
        return Vec::new();
    }
    if total <= cfg.max_chars {
        return vec![text.to_string()];
    }

    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < total {
        let mut end = (start + cfg.max_chars).min(total);
        // 句子边界优先：切点回退到最近的边界（保证至少推进一个字符）
        if end < total {
            if let Some(b) = (start + 1..end)
                .rev()
                .find(|&i| SENTENCE_BOUNDARIES.contains(&chars[i]))
            {
                end = b + 1;
            }
        }
        chunks.push(chars[start..end].iter().collect());
        // 最后一块已到文本末尾，结束
        if end == total {
            break;
        }
        // 重叠：下一块从 end - overlap 开始；重叠不能吞掉整个窗口
        start = end.saturating_sub(cfg.overlap_chars).max(start + 1);
    }
    chunks
}

/// 字符数（非字节数），测试用
fn char_len(s: &str) -> usize {
    s.chars().count()
}

// ---------- 单元测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ChunkConfig {
        ChunkConfig::new(20, 5)
    }

    #[test]
    fn short_text_not_split() {
        let chunks = split("短文本。", &cfg());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "短文本。");
    }

    #[test]
    fn empty_text_no_chunks() {
        assert!(split("", &cfg()).is_empty());
    }

    #[test]
    fn chunks_respect_max_chars() {
        let text = "第一句。第二句。第三句。第四句。第五句。第六句。第七句。第八句。";
        let chunks = split(text, &cfg());
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(char_len(c) <= 20, "块超长: {c:?}");
        }
    }

    #[test]
    fn chunks_have_overlap() {
        let text = "第一句。第二句。第三句。第四句。第五句。第六句。第七句。第八句。";
        let chunks = split(text, &cfg());
        for pair in chunks.windows(2) {
            let prev_tail: String = pair[0].chars().skip(pair[0].chars().count() - 5).collect();
            assert!(pair[1].contains(&prev_tail), "相邻块无重叠");
        }
    }

    #[test]
    fn no_text_lost() {
        let text = "第一句。第二句。第三句。第四句。第五句。第六句。第七句。第八句。";
        let chunks = split(text, &cfg());
        let joined: String = chunks.iter().flat_map(|c| c.chars()).collect();
        // 重叠会重复，但全文的句子都应出现且顺序不变
        let original: Vec<&str> = text.split('。').filter(|s| !s.is_empty()).collect();
        for s in original {
            assert!(joined.contains(s), "丢句: {s}");
        }
    }

    #[test]
    fn long_sentence_hard_split() {
        let long = "这是".repeat(30); // 60 字符无分隔符
        let chunks = split(&long, &cfg());
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(char_len(c) <= 20);
        }
    }
}

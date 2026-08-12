#!/usr/bin/env sh
# 按阶段提交代码。每个关键步骤一个独立 commit，方便回溯。
# 用法：bash scripts/commit-stages.sh
set -e
cd "$(dirname "$0")/.."

commit() {
  echo ""
  echo "==> $1"
  # 逐个 add：路径参数不带引号传入，这里分词逐个添加
  for f in $2; do
    git add "$f"
  done
  git commit -m "$3"
}

commit "阶段 1：基础模块（配置/错误/embedding）" \
  "Cargo.toml Cargo.lock examples/bench.rs src/config.rs src/error.rs src/embedding.rs" \
  "feat: 基础模块——配置、统一错误、embedding 专属线程（前缀/归一化/批量/预热）"

commit "阶段 2：服务端透明分块" \
  "src/chunker.rs" \
  "feat: 服务端内部分块（260 字符/块、50 重叠、句子边界优先）"

commit "阶段 3：向量索引封装" \
  "src/vector_index.rs" \
  "feat: IdMapIndex 封装——检索/删除/COW 序列化往返拷贝"

commit "阶段 4：存储层" \
  "src/store.rs" \
  "feat: tag↔块向量 1:N 存储、双快照原子持久化、损坏自愈重建"

commit "阶段 5：写者任务（读写互不阻塞）" \
  "src/writer.rs" \
  "feat: 写队列 + COW 快照发布，批量写合并持久化，读者无锁检索"

commit "阶段 6：HTTP API 与聚合检索" \
  "src/lib.rs src/search.rs src/api.rs src/main.rs" \
  "feat: HTTP API（upsert/批量/检索/删除/健康检查）+ 按 tag 聚合检索"

commit "阶段 7-8：优化与收尾" \
  "tests/e2e.rs README.md .gitignore scripts/" \
  "chore: 查询 LRU 缓存、端到端测试、README、提交脚本"

echo ""
echo "全部提交完成。验证：cargo test"

# Changelog

本项目的变更日志。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。
每个版本一个文件：`CHANGELOG-<版本>-<日期>.md`。

## [1.0.0] - 2026-08-12

首个可交付版本：tag（uuid）↔ 向量 的检索服务，供主 Agent 通过 HTTP 调用。
原始文档与 uuid 由 Agent 保管，本服务只做向量化、检索与删除。

### Added

- **核心 API**：单条 upsert（已存在则覆写）、批量 upsert、相似检索（返回 tag 列表 + 分数）、删除、健康检查（`/health`）
- **服务信息端点** `/info`：embedding 模型状态（就绪/名称/维度/参数量/线程数）、进程内存（RSS/VSZ）、向量规模
- **服务端透明分块**：长文（>512 token）自动分块（256 token/块、50 重叠、句子边界优先），Agent 契约始终为 tag ↔ 全文，无需在 Agent 端拆分
- **按 tag 聚合检索**：块级命中去重聚合（每 tag 取最高分），支持 `k` 与 `min_score` 过滤，查询文本自动加 bge 指令前缀
- **查询 embedding LRU 缓存**（默认 1024 条，命中免推理）
- **零外部依赖持久化**：`index.tvim` + `tags.bin` 双快照，临时文件 + rename 原子写；`.tvim` 损坏时从原始向量自愈重建
- **分阶段提交脚本** `scripts/commit-stages.sh`（按模块拆分历史提交）

### Changed

- **查询与写入互不阻塞**：COW 快照发布架构——读者持有不可变 `Arc<IndexSnapshot>` 无锁检索，写者独占可变状态、改完原子替换引用
- **批量写入优化**：llama.cpp 批量推理（多序列单次 encode）+ 整批只发布/持久化一次 + 批量 `add_with_ids`
- **推理线程化**：`LlamaContext` 为 `!Send`，采用专属 OS 线程 + channel 通信模型（后续写者任务复用同一模式）
- **向量处理规范**：插入与查询统一 L2 归一化、拒绝 NaN/Inf 输出
- **超长兜底**：单块超 512 token 截断并在响应中标记 `truncated`

### Fixed

- 多序列批量嵌入与单条嵌入结果不一致问题（批量 vs 单条余弦相似度 > 0.999，测试保障）
- llama.cpp encoder 批量解码断言崩溃（`n_ubatch >= n_tokens`）：分块预算改用 `n_ubatch`，截断改用模型原生长度而非被取整的 `n_ctx`
- 分块算法死循环与重叠不足：重写为字符窗口 + 句子边界回退，重叠精确可控
- turbovec 分数语义误判（相似度而非距离）导致的排序方向错误
- 嵌入线程模型加载失败时静默退出：改为降级模式，`/info` 可上报失败原因

### Performance

实测环境：8 核 CPU（AVX2）+ OpenBLAS，无 GPU。

| 指标 | 数值 |
|---|---|
| 查询端到端（100–200 字） | ~35–50 ms |
| 单条 embedding（4 线程，252 token） | 37.3 ms |
| 30 万向量检索 top-10 | 7.7 ms |
| COW 发布（30 万向量） | 53.6 ms/次 |
| 长文入库（5000 字 ≈ 20 块） | ~0.9 s |
| 空库启动内存（RSS） | ~79 MB |

### Docs

- `docs/architecture.md`：项目架构（组件、核心机制、决策记录）
- `docs/API.md`：正式 API 文档（含 `/info`）
- `docs/development.md`：开发说明
- `docs/deployment.md`：部署说明
- `README.md`：构建、运行、API 示例、数据文件说明

### 已知限制

1. **内存随库线性增长**：主要来自原始向量常驻（自愈能力换内存），10 万 tag 约 800 MB–1 GB；`RAG_STORE_RAW_VECTORS=false` 可省 ~600 MB，代价是 `.tvim` 损坏后需 Agent 重推全文
2. **tag 粒度 = 整篇文档**：长文命中返回整篇 tag，引用定位需 Agent 在自有文档中完成
3. **`n_ctx` 被 llama.cpp 向上取整至 4096**（`n_seq_max=16` 的副作用），多占 ~60 MB
4. **单机单进程**：数据在本机 `data/` 目录，不适合多实例横向扩展；备份直接复制目录

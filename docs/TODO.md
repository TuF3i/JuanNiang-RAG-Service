# TODO

> v3：Tag↔向量 检索服务（服务端透明分块、读写互不阻塞、批量写优化）。文档见 [技术文档.md](./技术文档.md)。

## 阶段 0：骨架与基准（P0）

- [ ] `Cargo.toml` 补齐依赖（llama-cpp-2、axum、tokio、serde、bincode、text-splitter、uuid、thiserror、tracing）
- [ ] `config.rs`：模型路径、数据目录、HTTP 端口、分块参数（256/50）、合并窗口
- [ ] `main.rs` 启动流程骨架（配置 → 模型 → 索引 → 写者任务 → 路由 → 监听）
- [ ] 下载/转换 `bge-small-zh-v1.5-q8_0.gguf` 到 `models/`
- [ ] **基准三件套**（`examples/bench.rs`）：
  - [ ] 单条 embedding 延迟（32 / 256 / 512 token 三档）
  - [ ] **线程数扫描**（1/2/4/6/8 线程嵌入延迟对比，最优值写入配置默认）
  - [ ] 30 万级向量检索延迟（合成数据，对应 10 万 tag × 3 块）
  - [ ] COW 发布耗时（序列化往返，30 万规模）
- [ ] 验收：跑通 Hello World API + 拿到本机基准数字，回填技术文档 §10

## 阶段 1：Embedding 模块（P0）

- [ ] `embedding.rs`：llama-cpp-2 加载 GGUF，`embedding = true`
- [ ] 查询指令前缀 `为这个句子生成表示以用于检索相关文章：`（仅查询加）
- [ ] L2 归一化 + 输入校验（拒绝 NaN / 非有限值）
- [ ] 查询超长兜底：>512 token 截断（查询通常短，仅兜底）
- [ ] 固定 `n_ctx = 512`（按 bge 上限配置，减少内存与初始化开销）
- [ ] 启动预热：加载完成后台跑一次短嵌入（跳过首请求懒初始化）
- [ ] 批量推理接口（多段文本一次 forward）
- [ ] 单 context + `Mutex`；批量分片（≤16 条/片）防拖垮查询
- [ ] 单元测试：归一化、前缀、截断边界、batch 形状
- [ ] 验收：单条/批量嵌入正确，延迟符合阶段 0 基准

## 阶段 2：服务端分块（P0）

- [ ] `chunker.rs`：text-splitter 配置（256 token / 50 overlap），**≤512 token 不切，直接单块**
- [ ] 中文段落/句子边界优先
- [ ] 单篇块数上限（建议 2000 块，可配置）
- [ ] 单元测试：短文不切、长文切块 token 分布、重叠正确、无文本丢失
- [ ] 验收：5000 字中文 ≈ 20 块（±20%）

## 阶段 3：向量索引封装（P0）

- [ ] `vector_index.rs`：`IdMapIndex::new(512, 4)`
- [ ] `add_with_ids` 批量添加、`search` → `(scores, ids)`、`remove(id)`
- [ ] **1:N 覆写**：删除 tag 下全部旧块 id → 批量 add 新块 id（验证 u64 id 稳定）
- [ ] **COW 拷贝**：`write` 临时文件 → `load` → `Arc`（验证往返一致）
- [ ] 发布后 `prepare()` 预热
- [ ] 索引重建：从原始向量批量重建 → `prepare()` → 写回 `.tvim`
- [ ] 验收：短文/长文 add → search → 覆写 → 删除 → 再 search 全链路正确

## 阶段 4：存储层（P0）

- [ ] `store.rs`：`TagStore`（tag_to_ids + next_id + 原始向量）
- [ ] `tags.bin` bincode 序列化（映射 + next_id + 原始归一化向量）
- [ ] 双快照原子写：临时文件 + rename（index.tvim + tags.bin）
- [ ] 启动加载 + 一致性校验（`index.len() == 块数总和`，不一致触发重建）
- [ ] `chunk_owner`（块 id → tag）启动时反查构建
- [ ] 单元测试：写→读→比对、缺文件/半截文件处理
- [ ] 验收：任意时刻 kill 进程，重启后数据完整

## 阶段 5：并发模型（P0，核心需求）

- [ ] `writer.rs`：写者任务 + `tokio mpsc` 队列
- [ ] 单条写：应用后立即发布（read-your-writes）
- [ ] 批量写：整批应用后**只发布一次**
- [ ] 发布机制：`RwLock<Arc<IndexSnapshot>>` 原子替换；读者瞬时取引用后无锁检索
- [ ] **验证互不阻塞**：并发压测——持续查询 + 批量写同时进行，查询延迟无尖峰
- [ ] 可选：合并窗口（50ms 内单条写合并发布）
- [ ] 验收：读写并发压测通过，查询 p99 不被写入拖垮

## 阶段 6：API 与聚合检索（P0）

- [ ] `api.rs`：
  - [ ] `PUT /tags/{tag}` upsert（响应含 `chunk_count`）
  - [ ] `POST /tags/batch` 批量 upsert
  - [ ] `GET /tags/search?q=&k=&min_score=` 返回 tag 列表 + 分数
  - [ ] `DELETE /tags/{tag}`
  - [ ] `GET /health`（tag 数 + 块向量数 + 快照版本）
- [ ] `search.rs`：查询流水线（嵌入 → 检索 top-k 块 → **按 tag 聚合去重** → 排序过滤）
- [ ] 聚合策略：默认每 tag 取最高分；可选 top-3 分数和（配置项）
- [ ] 错误处理与统一响应格式
- [ ] 验收：curl 走通全部端点；长文检索返回正确 tag；10 万 tag 规模延迟符合基准

## 阶段 7：优化（P1）

- [ ] 批量推理实测（1.5–3× 目标），调整分片大小
- [ ] 查询 embedding LRU 缓存（高频查询命中免推理，`lru` crate，默认 1024 条）
- [ ] 写入侧文本 hash 去重（批量内相同文本只嵌入一次，短期跨请求缓存）
- [ ] 读写 context 分离（各一个，彻底隔离推理串行点）
- [ ] 合并窗口落地与弱一致性说明
- [ ] 若单条写成为瓶颈：评估增量索引方案（LSM 式，见技术文档 §6.2）
- [ ] 长文本量大后评估 bge-m3 升级路线（技术文档 §11 方案 B）
- [ ] 验收：压测吞吐对比优化前后，记录到文档

## 阶段 8：健壮性与收尾（P1）

- [ ] 集成测试：短文/长文 upsert → 覆写 → search → delete → 重建 → 重启 全流程
- [ ] tracing 日志与可诊断错误
- [ ] 内存/磁盘占用抽样（验证 §10 量级）
- [ ] README 更新：构建、运行、API 示例、与主 Agent 的对接约定
- [ ] 验收：全新环境按 README 一条命令跑通

## 待定决策（实现前确认）

- [ ] 聚合策略默认值：每 tag 最高分 vs top-3 分数和（建议先 max，实测后调）
- [ ] `min_score` 默认值（建议 0.3–0.5，实测校准）
- [ ] tags.bin 是否存原始向量（体积 2–3×，换取 `.tvim` 损坏自愈；否则重建需 Agent 重推全文）
- [ ] 单条写 ack 时机：应用后 ack vs 发布后 ack
- [ ] 单篇块数上限（建议 2000）
- [ ] 是否暴露 `GET /tags` 全量列表（Agent 侧同步对账用）

# JuanNiang-RAG-Service API 文档

> 版本：v1（对应代码阶段 6）
> 服务地址：`http://{host}:{port}`，默认 `http://127.0.0.1:3000`
> 数据格式：全部 JSON（`Content-Type: application/json`）
> 契约：Agent 保管原始文档与 uuid，本服务只做向量化、检索与删除

## 目录

- [通用约定](#通用约定)
  - [分数语义](#分数语义)
  - [错误格式](#错误格式)
- [PUT /tags/{tag} —— 更新（upsert）](#put-tagstag--更新upsert)
- [POST /tags/batch —— 批量更新](#post-tagsbatch--批量更新)
- [GET /tags/search —— 检索](#get-tagssearch--检索)
- [DELETE /tags/{tag} —— 删除](#delete-tagstag--删除)
- [GET /health —— 健康检查](#get-health--健康检查)
- [示例：完整流程](#示例完整流程)
- [行为边界与注意事项](#行为边界与注意事项)

---

## 通用约定

### 分数语义

- 检索返回的 `score` 是**相似度**，范围 **[0, 1]**，越高越相似
- 完全相同的文本 ≈ 0.99+；语义相关通常 0.6–0.95；无关通常 < 0.6
- 由底层向量相似度映射而来（`(raw + 1) / 2`），阈值建议从 `0.5` 起步实测校准

### 错误格式

所有错误响应统一为：

```json
{ "error": "错误描述" }
```

| 状态码 | 含义 | 触发场景 |
|---|---|---|
| 400 | 参数错误 | `text`/`q` 为空、`items` 为空 |
| 404 | 资源不存在 | 删除不存在的 tag |
| 500 | 服务内部错误 | 模型未加载、存储损坏、嵌入失败等 |

---

## PUT /tags/{tag} —— 更新（upsert）

`tag` 已存在 → **覆写**其全部向量；不存在 → 新建。

```
PUT /tags/{tag}
```

| 位置 | 参数 | 说明 |
|---|---|---|
| path | `tag` | uuid，Agent 侧文档标识 |

**请求体**：

```json
{ "text": "要入库的文本内容" }
```

**响应 200**：

```json
{
  "tag": "3af2b489-b13a-42e4-af98-fe89d0e6b00e",
  "chunk_count": 4,
  "truncated": false
}
```

| 字段 | 说明 |
|---|---|
| `tag` | 入库的 tag（原样返回） |
| `chunk_count` | 实际入库的块数。≤512 token 为 1；长文自动分块（256 token/块）后 >1 |
| `truncated` | 是否存在被截断的块（单块超过模型上限 512 token 时，服务端截断处理） |

**curl 示例**：

```sh
curl -X PUT localhost:3000/tags/3af2b489-b13a-42e4-af98-fe89d0e6b00e \
  -H 'content-type: application/json' \
  -d '{"text": "Rust 是一种系统编程语言，强调内存安全与零成本抽象。"}'
```

**语义**：响应返回时数据**已持久化并发布**（读己之写），之后检索立即可见。

---

## POST /tags/batch —— 批量更新

多条文本一次批量 upsert：**一次嵌入推理 + 一次发布 + 一次持久化**（批量成本远低于逐条调用）。

```
POST /tags/batch
```

**请求体**：

```json
{
  "items": [
    { "tag": "11111111-1111-1111-1111-111111111111", "text": "第一条文本" },
    { "tag": "22222222-2222-2222-2222-222222222222", "text": "第二条文本" }
  ]
}
```

**响应 200**（逐条结果，顺序与请求一致；单条失败不影响其他条目）：

```json
{
  "results": [
    {
      "tag": "11111111-1111-1111-1111-111111111111",
      "chunk_count": 1,
      "truncated": false,
      "error": null
    },
    {
      "tag": "22222222-2222-2222-2222-222222222222",
      "chunk_count": 2,
      "truncated": false,
      "error": null
    }
  ]
}
```

| 字段 | 说明 |
|---|---|
| `error` | `null` 表示成功；否则为失败原因（如"文本为空"），该条目未入库 |
| 其余字段 | 同单条 upsert |

**curl 示例**：

```sh
curl -X POST localhost:3000/tags/batch \
  -H 'content-type: application/json' \
  -d '{"items": [{"tag": "11111111-1111-1111-1111-111111111111", "text": "第一段"}, {"tag": "22222222-2222-2222-2222-222222222222", "text": "第二段"}]}'
```

**建议**：Agent 侧做首次全量同步或批量更新时使用本端点；已成功的条目即使部分失败也会落盘。

---

## GET /tags/search —— 检索

输入文本，返回按相似度排序的 **tag 列表**（去重聚合，一个 tag 只出现一次）。

```
GET /tags/search?q=<文本>&k=<条数>&min_score=<阈值>
```

| 参数 | 必填 | 默认 | 说明 |
|---|---|---|---|
| `q` | ✅ | — | 查询文本（空文本返回 400） |
| `k` | | `10` | 返回条数，范围 1–100（超出自动钳制） |
| `min_score` | | 无 | 相似度下限过滤（0~1），低于该值的 tag 不返回 |

**响应 200**：

```json
{
  "results": [
    { "tag": "3af2b489-b13a-42e4-af98-fe89d0e6b00e", "score": 0.888 },
    { "tag": "22222222-2222-2222-2222-222222222222", "score": 0.698 }
  ]
}
```

**curl 示例**：

```sh
curl 'localhost:3000/tags/search?q=服务端透明分块与相似检索&k=5&min_score=0.5'
```

**说明**：

- 查询文本会自动加 bge 官方指令前缀（`为这个句子生成表示以用于检索相关文章：`），调用方无需处理
- 查询按 tag 聚合：一个 tag 有多个块命中时，取最高分块
- 命中缓存：相同的 `q` 短时间重复查询免推理（LRU，容量默认 1024）

---

## DELETE /tags/{tag} —— 删除

删除 tag 及其**全部**块向量。

```
DELETE /tags/{tag}
```

**响应 200**：

```json
{ "deleted": "3af2b489-b13a-42e4-af98-fe89d0e6b00e" }
```

**错误**：tag 不存在 → `404 { "error": "tag 不存在: <uuid>" }`

**curl 示例**：

```sh
curl -X DELETE localhost:3000/tags/3af2b489-b13a-42e4-af98-fe89d0e6b00e
```

---

## GET /health —— 健康检查

```
GET /health
```

**响应 200**：

```json
{ "status": "ok", "tags": 128, "chunks": 340 }
```

| 字段 | 说明 |
|---|---|
| `status` | `"ok"` 表示服务可用 |
| `tags` | 当前 tag 总数 |
| `chunks` | 当前块向量总数（长文分块后 chunks ≥ tags） |

**curl 示例**：

```sh
curl localhost:3000/health
```

---

## 示例：完整流程

```sh
# 1. 生成一个 tag
TAG=$(uuidgen)

# 2. 入库一篇长文（自动分块）
curl -X PUT localhost:3000/tags/$TAG -H 'content-type: application/json' \
  -d '{"text": "这是第一段。这是第二段。……（长文）"}'

# 3. 检索
curl "localhost:3000/tags/search?q=第二段的内容&k=3"

# 4. 不再需要时删除
curl -X DELETE localhost:3000/tags/$TAG
```

---

## 行为边界与注意事项

1. **文本长度**：单文本无上限（超长自动分块），但单块超过 512 token 会被截断并置 `truncated: true`——截断会损失尾部信息，Agent 侧应避免喂入超长单段
2. **空文本**：`text` 或 `q` 为空字符串（或纯空白）返回 400
3. **幂等性**：upsert 是幂等的——同 tag 重复提交相同文本，结果等价（内部先删旧块再写新块）
4. **一致性**：写接口返回即持久化完成；服务崩溃最多丢失"最后一次未返回的写入"
5. **并发**：检索与写入互不阻塞；批量写期间检索延迟不受影响
6. **单机限制**：数据全部在本机 `data/` 目录（`index.tvim` + `tags.bin`），备份直接复制该目录

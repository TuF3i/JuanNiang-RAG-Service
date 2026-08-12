//! 阶段 0 基准三件套：
//!   1. 单条 embedding 延迟 + 线程数扫描（需要模型文件，缺失则跳过）
//!   2. 30 万级向量检索延迟（合成数据，不依赖模型）
//!   3. COW 发布耗时（IdMapIndex 序列化往返）
//!
//! 运行：cargo run --release --example bench [模型路径]

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Instant;

const DIM: usize = 512; // bge-small-zh-v1.5 的输出维度
const BIT_WIDTH: usize = 4; // turbovec 每坐标量化位数
const N_VECTORS: usize = 300_000; // 约 10 万 tag × 平均 3 块
const REPEATS: usize = 20; // 每个测量取 20 次平均

fn main() {
    println!("===== RAG-Service 阶段 0 基准 =====");

    // 模型路径：第一个命令行参数，缺省用默认路径
    let model_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "models/bge-small-zh-v1.5-q8_0.gguf".to_string());

    if Path::new(&model_path).exists() {
        bench_embedding(&model_path);
    } else {
        println!("[跳过] 没找到模型文件: {model_path}");
        println!("       下载模型后：cargo run --release --example bench <模型路径>\n");
    }

    bench_search_and_cow();
}

// ---------- 基准 1：embedding 延迟 + 线程数扫描 ----------

fn bench_embedding(model_path: &str) {
    println!("----- 基准 1：单条 embedding 延迟 + 线程数扫描 -----");

    let backend = LlamaBackend::init().expect("初始化 llama backend 失败");
    let model = LlamaModel::load_from_file(&backend, model_path, &LlamaModelParams::default())
        .expect("加载模型失败，确认路径与 GGUF 文件");
    println!("模型加载成功，嵌入维度 n_embd_out = {}", model.n_embd_out());

    // 三档长度的中文文本：1 汉字 ≈ 1 token
    let base = "知识库检索服务测试句子。"; // 12 字 ≈ 12 token
    let samples = [
        ("短 ~36 token", base.repeat(3)),
        ("中 ~252 token", base.repeat(21)),
        ("长 ~504 token", base.repeat(42)),
    ];

    for threads in [1, 2, 4, 6, 8] {
        // 每个线程数单独建一个 context，线程数在 context 参数里设置
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(512)) // bge 上下文上限 512
            .with_embeddings(true) // 开启 embedding 模式
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let mut ctx = model
            .new_context(&backend, ctx_params)
            .expect("创建 context 失败");

        println!("线程数 = {threads}:");
        for (label, text) in &samples {
            embed_one(&mut ctx, &model, text); // 预热一次，排除首次懒初始化
            let start = Instant::now();
            for _ in 0..REPEATS {
                embed_one(&mut ctx, &model, text);
            }
            let avg_ms = start.elapsed().as_secs_f64() * 1000.0 / REPEATS as f64;
            println!("  {label:12} 平均 {avg_ms:6.1} ms/条");
        }
        println!();
    }
}

/// 单条文本 → 归一化向量。
/// 流程：分词 → 组 batch → decode → 读 embedding → 清 KV 缓存
fn embed_one(ctx: &mut LlamaContext<'_>, model: &LlamaModel, text: &str) -> Vec<f32> {
    let tokens = model.str_to_token(text, AddBos::Always).expect("分词失败");
    let mut batch = LlamaBatch::get_one(&tokens).expect("构造 batch 失败");
    ctx.decode(&mut batch).expect("decode 失败");
    // 先拷贝出来，才能清空缓存（emb 借用着 ctx，clear 需要独占修改它）
    let emb = ctx
        .embeddings_seq_ith(0)
        .expect("读取 embedding 失败")
        .to_vec();
    ctx.clear_kv_cache();
    normalize(&emb)
}

/// L2 归一化：每个分量除以向量长度
fn normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / norm).collect()
}

// ---------- 基准 2 & 3：检索 + COW（合成数据，不需要模型） ----------

fn bench_search_and_cow() {
    use turbovec::IdMapIndex;

    println!("----- 基准 2：{N_VECTORS} 向量检索延迟（合成数据） -----");
    println!(
        "（合成数据约占 {} MB 内存，稍等）",
        N_VECTORS * DIM * 4 / 1024 / 1024
    );

    let mut index = IdMapIndex::new(DIM, BIT_WIDTH).expect("创建索引失败");
    let ids: Vec<u64> = (0..N_VECTORS as u64).collect();
    let vectors = random_normalized_vectors(N_VECTORS, DIM);

    let t = Instant::now();
    index.add_with_ids(&vectors, &ids).expect("批量添加失败");
    println!(
        "  添加 {N_VECTORS} 个向量：{:.0} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    drop(vectors); // 数据已进索引，释放 600MB 原始数据
    index.prepare(); // 预热检索缓存（旋转矩阵/码本/SIMD 布局）

    let query = random_normalized_vectors(1, DIM);
    index.search(&query, 10); // 预热
    let t = Instant::now();
    for _ in 0..REPEATS {
        let (scores, hit_ids) = index.search(&query, 10);
        assert_eq!(hit_ids.len(), 10);
        assert_eq!(scores.len(), 10);
    }
    let avg = t.elapsed().as_secs_f64() * 1000.0 / REPEATS as f64;
    println!("  单查询 top-10：平均 {avg:.2} ms");

    println!("\n----- 基准 3：COW 发布耗时（write → load 序列化往返） -----");
    std::fs::create_dir_all("data").expect("创建 data 目录失败");
    let cow_path = "data/bench_cow.tvim";
    let t = Instant::now();
    index.write(cow_path).expect("序列化失败");
    let cloned = IdMapIndex::load(cow_path).expect("反序列化失败");
    println!(
        "  COW 拷贝：{:.1} ms（新快照含 {} 个向量）",
        t.elapsed().as_secs_f64() * 1000.0,
        cloned.len()
    );
    let _ = std::fs::remove_file(cow_path);
}

/// 生成 n 个 dim 维随机单位向量（turbovec 假设输入已归一化）
fn random_normalized_vectors(n: usize, dim: usize) -> Vec<f32> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut v = vec![0.0f32; n * dim];
    for row in v.chunks_exact_mut(dim) {
        for x in row.iter_mut() {
            *x = rng.gen_range(-1.0..1.0);
        }
        let norm: f32 = row.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in row.iter_mut() {
            *x /= norm;
        }
    }
    v
}

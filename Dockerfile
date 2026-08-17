# syntax=docker/dockerfile:1

# ========== 构建阶段 ==========
# 说明：
# - llama.cpp 由构建脚本编译 C++ 源码，需要 cmake + make + C++ 编译器 + OpenBLAS；
#   bindgen 生成 FFI 绑定还需 libclang（见下方 LIBCLANG_PATH）
# - 首次构建 5-20 分钟属正常；依赖层已独立缓存，改业务代码只增量编译
# - 基础镜像使用国内加速域名（docker.jiaxin.site/library/ 对应 Docker Hub 官方镜像）
FROM docker.jiaxin.site/library/rust:1-slim-bookworm AS builder

# 国内镜像加速：apt 换清华源，cargo 换清华 sparse 源
RUN sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
        /etc/apt/sources.list /etc/apt/sources.list.d/debian.sources 2>/dev/null || true \
    && mkdir -p "$CARGO_HOME" \
    && printf '[source.crates-io]\nreplace-with = "tuna"\n\n[source.tuna]\nregistry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"\n' > "$CARGO_HOME/config.toml" \
    && apt-get update && apt-get install -y --no-install-recommends \
        cmake \
        make \
        g++ \
        pkg-config \
        libopenblas-dev \
        libclang-dev \
    && rm -rf /var/lib/apt/lists/*

# llama-cpp-sys-2 构建时用 bindgen 生成 FFI 绑定，必须能找到 libclang（bookworm 自带 LLVM 14）
ENV LIBCLANG_PATH=/usr/lib/llvm-14/lib

WORKDIR /build

# 先拷贝清单 + 占位源码：把最慢的依赖编译（含 llama.cpp C++）缓存成独立层
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release \
    && rm -rf src

# 拷贝真实源码，增量编译业务代码
COPY src ./src
RUN cargo build --release

# ========== 运行阶段 ==========
# 说明：
# - 模型随镜像分发：构建时从上下文 models/ 拷入（需先准备模型，
#   可用 scripts/download_model.py 从魔搭社区下载）；
#   运行时仍可挂载卷覆盖 /app/models
# - 数据目录 /app/data 建议挂载卷持久化
# - 基础镜像使用国内加速域名
FROM docker.jiaxin.site/library/debian:bookworm-slim AS runtime

# 国内镜像加速：apt 换清华源
RUN sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
        /etc/apt/sources.list /etc/apt/sources.list.d/debian.sources 2>/dev/null || true \
    && apt-get update && apt-get install -y --no-install-recommends \
        libopenblas0 \
        libgomp1 \
        wget \
    && rm -rf /var/lib/apt/lists/*

# 非 root 运行
RUN useradd --create-home --shell /usr/sbin/nologin rag

WORKDIR /app
COPY --from=builder /build/target/release/JuanNiang-RAG-Service /usr/local/bin/rag-service

# 模型随镜像分发：从构建上下文 models/ 拷入（约 25MB）
COPY models/ /app/models/

RUN mkdir -p /app/data && chown -R rag:rag /app
USER rag

# 服务配置（可用环境变量覆盖，见 src/config.rs）
ENV RAG_MODEL_PATH=/app/models/bge-small-zh-v1.5-q8_0.gguf \
    RAG_DATA_DIR=/app/data \
    RAG_HOST=0.0.0.0 \
    RAG_PORT=3000 \
    RAG_N_THREADS=4

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD wget -qO- http://127.0.0.1:3000/health >/dev/null 2>&1 || exit 1

VOLUME ["/app/data"]
ENTRYPOINT ["/usr/local/bin/rag-service"]

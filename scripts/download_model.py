#!/usr/bin/env python3
"""从魔搭社区（ModelScope）下载模型文件（默认找 .gguf）。

零第三方依赖（仅 Python 标准库）。

用法：
    python3 scripts/download_model.py --model-id <模型ID>
    python3 scripts/download_model.py --model-id <模型ID> --file 指定文件名.gguf
    python3 scripts/download_model.py --model-id <模型ID> --output models/ --force

示例（在魔搭搜索 "bge-small-zh-v1.5 gguf" 获取模型 ID）：
    python3 scripts/download_model.py --model-id 某用户/bge-small-zh-v1.5-gguf
"""

import argparse
import hashlib
import json
import os
import sys
import urllib.request

# 魔搭 API 端点
FILES_API = "https://modelscope.cn/api/v1/models/{model_id}/repo/files?Revision={rev}&Recursive=true"
RESOLVE_URL = "https://modelscope.cn/models/{model_id}/resolve/{rev}/{path}"


def api_get(url: str) -> dict:
    """GET 并解析 JSON，带基本错误处理。"""
    try:
        with urllib.request.urlopen(url, timeout=30) as resp:
            return json.load(resp)
    except Exception as e:  # noqa: BLE001 - 统一转为用户可读错误
        sys.exit(f"[错误] 请求魔搭 API 失败: {e}\n       URL: {url}")


def list_gguf_files(model_id: str, revision: str) -> list[dict]:
    """列出模型仓库里的 .gguf 文件（返回 [{path, size}]）。"""
    data = api_get(FILES_API.format(model_id=model_id, rev=revision))
    files = (data.get("Data") or {}).get("Files") or []
    gguf = [f for f in files if str(f.get("Path", "")).lower().endswith(".gguf")]
    if not gguf:
        # 打印响应结构便于排查（API 字段可能随版本变化）
        sys.exit(
            f"[错误] 在 {model_id} 中未找到 .gguf 文件。\n"
            f"       原始响应片段: {json.dumps(data, ensure_ascii=False)[:300]}"
        )
    return gguf


def download(url: str, dest: str, force: bool = False) -> None:
    """分块下载到 dest；文件已存在且非 force 时跳过。"""
    if os.path.exists(dest) and not force:
        print(f"[跳过] {dest} 已存在（--force 可覆盖）")
        return

    os.makedirs(os.path.dirname(dest) or ".", exist_ok=True)
    print(f"[下载] {url}")
    with urllib.request.urlopen(url, timeout=60) as resp, open(dest, "wb") as f:
        total = int(resp.headers.get("Content-Length", 0))
        done = 0
        while chunk := resp.read(1024 * 256):
            f.write(chunk)
            done += len(chunk)
            if total:
                pct = done * 100 // total
                print(f"\r[进度] {done / 1024 / 1024:.1f} / {total / 1024 / 1024:.1f} MB ({pct}%)", end="", flush=True)
    print(f"\n[完成] {dest}（{os.path.getsize(dest) / 1024 / 1024:.1f} MB）")


def verify_sha256(path: str, expected: str) -> None:
    """下载后校验 SHA-256（可选）。"""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1024 * 1024):
            h.update(chunk)
    actual = h.hexdigest()
    if actual != expected.lower():
        sys.exit(f"[错误] SHA-256 校验失败: 期望 {expected}，实际 {actual}")
    print(f"[校验] SHA-256 一致: {actual}")


def main() -> None:
    parser = argparse.ArgumentParser(description="从魔搭社区下载 GGUF 模型")
    parser.add_argument("--model-id", required=True, help="魔搭模型 ID，如 用户/模型名")
    parser.add_argument("--file", default=None, help="指定文件名；缺省自动找仓库内第一个 .gguf")
    parser.add_argument("--revision", default="master", help="版本（默认 master）")
    parser.add_argument("--output", default="models", help="保存目录（默认 models/）")
    parser.add_argument("--force", action="store_true", help="覆盖已存在的文件")
    parser.add_argument("--sha256", default=None, help="可选：下载后校验 SHA-256")
    args = parser.parse_args()

    if args.file:
        files = [{"Path": args.file, "Size": 0}]
    else:
        print(f"[查找] {args.model_id} 中的 .gguf 文件…")
        files = list_gguf_files(args.model_id, args.revision)
        for f in files:
            print(f"       - {f['Path']}（{f.get('Size', 0) / 1024 / 1024:.1f} MB）")
        if len(files) > 1:
            print(f"[提示] 共 {len(files)} 个 .gguf，默认下载第一个；可用 --file 指定")

    target = files[0]["Path"]
    dest = os.path.join(args.output, os.path.basename(target))
    url = RESOLVE_URL.format(model_id=args.model_id, rev=args.revision, path=target)
    download(url, dest, args.force)

    if args.sha256:
        verify_sha256(dest, args.sha256)


if __name__ == "__main__":
    main()

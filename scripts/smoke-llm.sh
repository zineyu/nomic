#!/usr/bin/env bash
# 真实 provider 冒烟：一轮对话 + 一次工具调用。
# 手动运行，不进 CI 门禁。
#
#   NOMIC_API_KEY=... NOMIC_MODEL=... [NOMIC_BASE_URL=...] scripts/smoke-llm.sh
set -euo pipefail

: "${NOMIC_API_KEY:?请设置 NOMIC_API_KEY}"
: "${NOMIC_MODEL:?请设置 NOMIC_MODEL}"

cd "$(dirname "$0")/.."
exec devenv shell -- cargo run -p nomic-ai --example smoke

#!/usr/bin/env bash
# 文件行数检查（棘轮式 size gate）
#
# 规则：
#   1. crates/ 下所有 .rs 文件不得超过 MAX_LINES 行；
#   2. 历史超限文件登记在 BASELINE 中（路径 + 登记时行数），只许缩小、不许增长；
#   3. 文件缩小到 MAX_LINES 以内后，必须同步移除 BASELINE 中的条目（棘轮只进不退）。
#
# 新增豁免需评审同意：把文件当前行数追加到 BASELINE（保持按路径排序）。
set -euo pipefail

cd "$(dirname "$0")/.."

MAX_LINES=800
BASELINE="scripts/file-size-baseline.txt"

fail=0

# 读取 baseline：每行 "<路径><TAB><登记行数>"，# 开头为注释
declare -A baseline=()
if [[ -f "$BASELINE" ]]; then
  while IFS=$'\t' read -r path lines; do
    [[ -z "${path:-}" || "$path" == \#* ]] && continue
    baseline["$path"]="$lines"
  done < "$BASELINE"
fi

while IFS= read -r file; do
  lines=$(wc -l < "$file")
  if (( lines > MAX_LINES )); then
    if [[ -v "baseline[$file]" ]]; then
      limit=${baseline[$file]}
      if (( lines > limit )); then
        echo "❌ $file: $lines 行，超过登记基线 $limit 行（只许缩小，不许增长）" >&2
        fail=1
      fi
    else
      echo "❌ $file: $lines 行，超过上限 $MAX_LINES 行" >&2
      echo "   请拆分文件；确需豁免时在评审同意后登记到 $BASELINE" >&2
      fail=1
    fi
  fi
done < <(find crates -name '*.rs' | sort)

# baseline 条目必须保持“新鲜”：文件已删除或已缩小到上限以内时强制清理
for path in "${!baseline[@]}"; do
  if [[ ! -f "$path" ]]; then
    echo "❌ $BASELINE 中的 $path 已不存在，请移除该条目" >&2
    fail=1
  elif (( $(wc -l < "$path") <= MAX_LINES )); then
    echo "❌ $path 已缩小到 $MAX_LINES 行以内，请从 $BASELINE 移除该条目（棘轮只进不退）" >&2
    fail=1
  fi
done

if (( fail )); then
  echo "" >&2
  echo "文件行数检查未通过（上限 $MAX_LINES 行，豁免见 $BASELINE）" >&2
  exit 1
fi

echo "✅ 文件行数检查通过（上限 $MAX_LINES 行，豁免 ${#baseline[@]} 个）"

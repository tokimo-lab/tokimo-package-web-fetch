#!/usr/bin/env bash
# 把 core.hooksPath 配为本仓库内的 .githooks/。
# 由主仓库 (tokimo) 同步过来，单独 clone 本仓库时跑此脚本启用 fmt hook。
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
if [ ! -f .githooks/pre-commit ]; then
  echo "✗ 缺少 .githooks/pre-commit" >&2
  exit 1
fi
chmod +x .githooks/pre-commit
git config core.hooksPath .githooks
echo "✓ pre-commit fmt hook installed (core.hooksPath = .githooks)"

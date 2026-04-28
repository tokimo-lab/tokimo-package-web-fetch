#!/usr/bin/env bash
# 启用本仓库的 pre-commit fmt 钩子（Lefthook）。
# 由 tokimo 主仓库同步过来。单独 clone 本仓库时跑此脚本启用 hook。
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

if command -v lefthook >/dev/null 2>&1; then
  LEFTHOOK="$(command -v lefthook)"
elif [ -x "../../node_modules/.bin/lefthook" ]; then
  LEFTHOOK="$(cd ../.. && pwd)/node_modules/.bin/lefthook"
else
  echo "✗ 找不到 lefthook，请安装其一：" >&2
  echo "    npm i -g lefthook" >&2
  echo "    brew install lefthook" >&2
  echo "    cargo install lefthook" >&2
  echo "    scoop install lefthook" >&2
  exit 1
fi

"$LEFTHOOK" install
echo "✓ pre-commit fmt hook installed"

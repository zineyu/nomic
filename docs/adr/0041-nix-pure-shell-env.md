# ADR-0041: nix 纯净 shell 环境（workspace 级 bash 执行器）

- 状态：草案
- 日期：2026-09-01

## 背景

bash 工具（`nomic-tools/src/bash.rs`）直接 spawn 宿主机 `bash -c`，agent 可
用的命令完全取决于宿主环境：不同机器、不同 workspace 的工具集不一致，
宿主 PATH 里的脏环境（代理变量、语言版本管理器 shim）也会泄漏进 agent
命令。nomic 需要一个**按 workspace 定义、可复现、agent 可自服务修改**的
命令执行环境。

已确认的方向（与需求方讨论定稿）：

1. 环境定义为 workspace `.nomic/` 下的 **flake devShell**，执行语义对应
   `nix develop --ignore-environment`（pure 模式；`nix shell --pure` 语义
   是安装包列表，mkShell 型定义的对应命令实为 `nix develop`）。
2. 执行器采用**会话级 env 缓存**：首次使用或定义变更后用 nix 解析一次
   环境，之后 plain bash + 注入缓存 env——`nix develop` 求值有数百 ms～
   秒级延迟，不能摊到每次 bash 调用上。
3. agent 经 `nix://shell` URI（新 ProtocolHandler，可读写）编辑定义文件。
4. nix 不可用 / 定义缺失 / 求值失败时**静默回退 plain bash**，在结果
   details 与输出尾注中提示，可用性优先。

## 决策

### 环境定义：`<workspace>/.nomic/flake.nix`

- 单文件 flake，`outputs.devShells.<system>.default = pkgs.mkShell { … }`。
- **惰性创建**：workspace 首次初始化（CLI session 绑定 workspace、web
  登记 workspace）且 `nix` 可用时生成默认模板；nix 不可用则不创建。
  创建逻辑 `ensure_default_flake(workspace)` 位于 `nomic-tools::nix_env`，
  调用点在 `nomic-cli`（`bootstrap.rs` workspace 确定后、
  `web/workspace.rs` 登记处）。
- 默认模板包集：`bashInteractive`、`jq`、`curl`、`git`、`gh`（coreutils
  等基础工具由 stdenv 隐式提供）。
- **git 可见性**：flake 在 git 仓库内只对已跟踪（或 intent-to-add）文件
  生效。`ensure_default_flake` 写入模板后，若 workspace 在 git 仓库内，
  best-effort 执行 `git add -N -- .nomic/flake.nix`；求值失败信息包含
  "untracked flake" 特征时在回退提示中引导用户 `git add` 该文件。

### 执行器：env 缓存 + 注入式 bash

新模块 `nomic-tools/src/nix_env.rs`：

```
NixEnvCache（每会话一个 Arc，随工具集构造注入 BashTool）
  ├─ resolve(workspace)：单飞（tokio Mutex）解析
  │    缓存键 = (flake.nix mtime, flake.lock mtime)
  │    每次 bash 调用先 stat 两个文件，键未变 → 直接命中
  │    miss → nix develop --ignore-environment path:<.nomic> --command env -0
  │           解析 NUL 分隔环境为 HashMap；HOME/USER/TMPDIR 缺失时从
  │           宿主 env 补齐（pure 模式会清掉，但工具需要可写的 HOME）
  ├─ 解析失败 → 缓存 Unavailable(reason)，文件不变不重试；
  │    bash 回退宿主环境并附尾注
  └─ nix 本身不可用（which nix / 特性开关缺失）→ 同样 Unavailable
```

- BashTool 新增 `with_nix_env(Arc<NixEnvCache>)`；execute 时：
  `Ok(env)` → `Command::new("bash").env_clear().envs(env)`（bash 由缓存
  PATH 解析，默认模板含 bashInteractive）；`Err(reason)` → 现有 plain
  spawn 路径不变，结果 `details.nix = { mode, reason }` 并在输出尾部附
  一行提示（与截断 notice 同款风格）。
- 进程组强杀、超时、截断等既有行为不受影响（只改 spawn 前的 env 注入）。
- 工具 description 增补一句：命令在 workspace 的 nix 环境（`.nomic/flake
  .nix` 定义，经 `nix://shell` 可编辑）中执行，不可用时回退宿主环境。

### `nix://shell` URI

新 handler `nomic-uri/src/handlers/nix.rs`：

- 仅一个资源 `nix://shell` → `<workspace>/.nomic/flake.nix`；其余路径
  报 Resolve 错误并列出可用资源。
- 可读写（`writable() = true`）：write 创建父目录后落盘；下次 bash 调用
  经 mtime 检查自然失效重解析，**无需跨组件通知**。
- `source_path` 指向真实文件（grep/bash 对齐）；`complete()` 返回
  `["shell"]`；immutable = false。
- resolve 时文件缺失：返回错误并提示"write nix://shell 即创建（默认
  模板内容见 BashTool 初始化路径）"。

### 回退矩阵

| 情况 | 行为 |
|---|---|
| nix 不在 PATH / 无 flakes 特性 | plain bash，尾注 + details 说明 |
| `.nomic/flake.nix` 缺失且无法惰性创建 | plain bash（不提示，等同现状） |
| flake 求值/devShell 构建失败 | plain bash，尾注含 nix 错误摘要 |
| flake 在 git 仓库且未跟踪 | 同上，提示引导 `git add -N` |

### 测试

- 纯函数单测：`env -0` 解析、HOME/USER 补齐、缓存键（mtime）比较。
- `nix://` handler 单测（仿 local.rs，tempdir，不需要 nix）：读写往返、
  未知路径报错、补全。
- 集成测试（`command -v nix` 缺失则 skip，devenv 内恒有 nix）：默认模板
  可解析且 `bash`/`jq` 在 PATH；flake 文件 mtime 变更触发重解析；坏
  flake 回退 plain bash 且输出含尾注。
- 既有 bash 工具测试不受影响（默认不注入 NixEnvCache 时行为完全不变）。

## 非目标

- 常驻 shell 进程（跨调用状态泄漏，生命周期复杂，明确放弃）。
- `devenv` / `direnv` 集成；`.nomic/flake.nix` 是 nomic 自有格式。
- `nix://status`、`nix://packages` 等派生只读资源（YAGNI，需要时再按
  本 ADR 模式追加）。
- 非 unix 平台（进程组语义本就 unix-only；nix 支持范围随其）。

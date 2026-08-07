# 版本发布流程

nomic 所有版本（patch / minor / major）统一走**本地脚本 + PR 发版**：

1. 本地 devenv `release` 脚本完成 bump、CHANGELOG、门禁并生成 release 分支；
2. release 分支经 **PR 合并**进入 main（main 受 ruleset 保护，必须走 PR）；
3. 合并后 GitHub Action 自动在 main 上打 tag、执行门禁、多平台构建并发布
   GitHub Release。

## 前置条件

- 在 devenv shell 中操作（`devenv shell` 或 direnv 自动进入），`release` 脚本、
  `cargo set-version`、`git-cliff` 均由 devenv 提供，版本锁定在 `devenv.lock`。
- commit message 遵循 Conventional Commits（`feat:` / `fix:` / `docs:` …），
  CHANGELOG 与 Release notes 由 git-cliff 据此生成（分组规则见
  [cliff.toml](../cliff.toml)）。

## 发布步骤

```bash
# 1. 确保在 main 最新提交上、工作副本干净
jj new main

# 2. 执行发布脚本（以 0.2.0 为例，不带 v 前缀）
release 0.2.0
```

脚本会依次执行，任一步失败即中止：

1. **前置校验**：版本号合法性、tag 未占用、工作副本为空、基于 `main` 最新提交；
2. **bump 版本**：`cargo set-version --workspace` 更新 `Cargo.toml`（workspace 统一
   版本）与 `Cargo.lock`；
3. **生成 CHANGELOG**：`git-cliff --tag v0.2.0 -o CHANGELOG.md`，将上个 tag 以来的
   conventional commits 归入新版本；
4. **完整检查**：运行 `check`（与 CI 等价；`RELEASE_SKIP_CHECK=1` 可跳过，不推荐）；
5. **提交到 release 分支**：生成 `chore(release): v0.2.0` 提交并放置
   `release/v0.2.0` 书签。**此阶段不打 tag、不动 main**。

```bash
# 3. 推送分支并创建 PR（脚本最后也会打印这两条命令）
jj git push --bookmark release/v0.2.0
gh pr create --base main --head release/v0.2.0 \
  --title 'chore(release): v0.2.0'

# 4. PR 检查（devenv check / nix flake check）全绿后合并；
#    merge / squash / rebase 任意方式均可
```

## 合并后自动完成的部分

**第一步：[release-tag.yml](../.github/workflows/release-tag.yml)**（push to main 触发）
检测到头提交是 `chore(release): vX.Y.Z` 且 workspace 版本一致、tag 不存在时：

1. 在 main 的合并提交上创建附注 tag `vX.Y.Z` 并推送；
2. 以 `--ref vX.Y.Z` 派发 `release.yml`。

> `GITHUB_TOKEN` 推送的 tag 不会级联触发 workflow（GitHub 官方限制），
> `workflow_dispatch` 是例外，因此「打 tag」与「触发发布」由同一 workflow 完成。

**第二步：[release.yml](../.github/workflows/release.yml)**：

1. **门禁**：与 `ci.yml` 完全相同的 `devenv check` 与 `nix flake check`，并校验 tag
   版本与 workspace 版本一致，防止把未通过检查的提交发布出去；
2. **构建**：4 个目标平台的 release 二进制（均为原生 runner，无交叉编译）：

   | target | runner |
   | --- | --- |
   | `x86_64-unknown-linux-gnu` | `ubuntu-latest` |
   | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` |
   | `x86_64-apple-darwin` | `macos-15-intel` |
   | `aarch64-apple-darwin` | `macos-latest` |

3. **发布**：`git-cliff --current` 生成该版本的 Release notes，将 tar.gz 与 SHA256
   校验和上传至 GitHub Release。

## 逃生门：本地手动推 tag

ruleset 只约束 main 分支，不约束 tag。紧急场景（如 release-tag.yml 故障）可
在 release PR 合并后手动完成打 tag 与触发：

```bash
jj new main
git tag -a v0.2.0 -m v0.2.0
git push origin v0.2.0   # tag 推送直接触发 release.yml
```

tag 推送是 `release.yml` 的合法触发方式，行为与自动派发完全等价。
**不要复用已推送的 tag**；发错版本用新版本号重发。

## 产物与安装

- **GitHub Releases**：`nomic-<tag>-<target>.tar.gz`（含二进制、README、CHANGELOG）
  及对应 `.sha256`；
- **Nix**：tag 即 flake ref，`nix profile install github:zineyu/nomic/v0.2.0` 可安装
  指定版本；`nix flake check` 保证每个 tag 的 flake 可用；
- **crates.io**：不发布（内部 workspace，安装走 nix 或预编译二进制）。

## 预告下一个版本的内容

```bash
git cliff --unreleased   # 预览上个 tag 以来将写入 CHANGELOG 的条目
```

## 故障处理

- **`release` 脚本失败（PR 创建前）**：远端无任何改动。修复后 `jj restore` 清理
  工作副本再重跑；脚本已通过完整 `check`，此处失败多为环境/网络问题。
- **PR 检查失败**：release commit 未进 main，无 tag 无 Release。在 release 分支上
  修复（`jj squash` 进 release commit 或追加修复提交），force push 分支即可。
- **release-tag.yml 失败**：main 已有 release commit，但 tag/Release 未创建。
  - 打 tag 前失败（如检测逻辑异常）：修复后直接 **Re-run** 该 workflow；
  - tag 已推送但 dispatch 失败：手动 `gh workflow run release.yml --ref vX.Y.Z`。
- **release.yml 失败**：tag 与 main 均已就位，main 不受影响。
  - 瞬时问题（crates.io 抖动、runner 异常）：Actions 页面 **Re-run failed jobs**；
    release job 的 `action-gh-release` 幂等，重跑可补齐部分上传的产物；
  - 代码问题（概率低：同一提交已过本地 `check` + PR CI 两道相同门禁）：在
    GitHub 删除 Release 与远端 tag（`git push origin :vX.Y.Z`），修复后用
    **新版本号**重新走发布流程。
- **tag 打错（手动逃生门场景）**：未推送时 `git tag -d vX.Y.Z`；已推送则删除
  Release 与远端 tag，修复后用新版本号重发，不要复用已推送的 tag。
- **aarch64-linux runner 不可用**（私有仓库无 ARM runner 额度）：将构建矩阵中对应
  条目改为 `ubuntu-latest` + `cross` 交叉编译。

# 版本发布流程

nomic 采用**混合发版模式**，按版本号级别分两条路径：

| 版本级别 | 方式 | 入口 |
| --- | --- | --- |
| **patch**（最小版本号，如 0.1.1 → 0.1.2） | GitHub Action 全自动 | 手动运行 `Release (patch)` workflow |
| **minor / major**（如 0.1.x → 0.2.0） | 本地手动 | devenv `release` 脚本 + 推 tag |

两条路径共用同一套门禁（`devenv check` + `nix flake check`）、同一套多平台
构建矩阵与同一份 CHANGELOG 生成逻辑（git-cliff + conventional commits，
分组规则见 [cliff.toml](../cliff.toml)）。

## patch 版本：GitHub Action 自动发版

在 GitHub 页面 **Actions → Release (patch) → Run workflow**（在 `main` 上运行），
由 [release-patch.yml](../.github/workflows/release-patch.yml) 全自动完成：

1. **prepare**：checkout `main` → `cargo set-version --bump patch` 自动递增最小
   版本号 → git-cliff 生成 CHANGELOG → 提交 `chore(release): vX.Y.Z` → 推送
   `release/vX.Y.Z` 分支。**此阶段不打 tag、不动 main**；
2. **check / nix**：与 `ci.yml` 完全等价的门禁，在 release 提交上运行；
3. **build**：4 个目标平台的 release 二进制（矩阵见下文）；
4. **release**：创建 tag `vX.Y.Z` 与 GitHub Release，上传 tar.gz 与 SHA256；
5. **finalize**：**仅当发布成功**，创建 `release/vX.Y.Z` → `main` 的 PR 将 bump
   提交合入 main（尝试开启自动合并；不可用时等待人工合并）。

关键行为：

- **main 只通过 PR 更新，不直接推送**。任一步失败，`main` 与远端 tag 均保持原样；
  修复问题后直接重新 Run workflow 即可——版本号会从 `main` 重新计算为同一
  值，`release/vX.Y.Z` 分支被 `--force` 覆盖；PR 创建是幂等的，重跑会复用
  已存在的 PR。
- **请用 merge commit 合并发版 PR**。tag 指向 release 分支上的提交，squash /
  rebase 合并会改写提交，使 tag 指向的提交不在 main 历史中。
- workflow 使用 `GITHUB_TOKEN` 创建 tag 与 PR，**不会**再次触发 tag 推送的
  `release.yml`；但 `GITHUB_TOKEN` 创建的 PR 也不会自动触发 `ci.yml`（release
  提交刚通过完全相同的门禁）。若分支保护要求 PR 上必须有检查通过才能合并，
  需在保护规则中允许 GitHub Actions bot 绕过，或为 workflow 配置 PAT。
- **dry_run 输入**：勾选后只执行 prepare/check/nix/build，不创建 Release、
  不创建 PR，用于验证流程改动（可在非 main 分支上运行）。
- 非 main 分支上的运行只能配合 dry_run 验证；真正发布必须从 main 触发。
- PR 合并后 release 分支由 GitHub「自动删除 head 分支」仓库设置清理，未开启
  时可手动删除。

## minor / major 版本：本地手动发版

本地 `release` 脚本已内置守卫：**拒绝 patch 递增**（与当前版本 major.minor
相同的目标版本），patch 请走上面的 Action；确需本地发 patch 时设
`RELEASE_ALLOW_PATCH=1` 绕过。

### 前置条件

- 在 devenv shell 中操作（`devenv shell` 或 direnv 自动进入），`release` 脚本、
  `cargo set-version`、`git-cliff` 均由 devenv 提供，版本锁定在 `devenv.lock`。
- commit message 遵循 Conventional Commits（`feat:` / `fix:` / `docs:` …）。

### 发布步骤

```bash
# 1. 确保在 main 最新提交上、工作副本干净
jj new main

# 2. 执行发布脚本（以 0.2.0 为例）
release 0.2.0
```

脚本会依次执行，任一步失败即中止：

1. **前置校验**：版本号合法性、tag 未占用、非 patch 递增（见守卫说明）、
   工作副本为空、基于 `main` 最新提交；
2. **bump 版本**：`cargo set-version --workspace` 更新 `Cargo.toml`（workspace 统一
   版本）与 `Cargo.lock`；
3. **生成 CHANGELOG**：`git-cliff --tag v0.2.0 -o CHANGELOG.md`，将上个 tag 以来的
   conventional commits 归入新版本；
4. **完整检查**：运行 `check`（与 CI 等价；`RELEASE_SKIP_CHECK=1` 可跳过，不推荐）；
5. **提交并打 tag**：生成 `chore(release): v0.2.0` 提交，移动 `main` 书签，创建
   附注 tag `v0.2.0`。

```bash
# 3. 人工确认后推送（脚本最后也会打印这两条命令）
jj git push --bookmark main
git push origin v0.2.0
```

### CI 自动完成的部分

tag 推送触发 [release.yml](../.github/workflows/release.yml)：

1. **门禁**：与 `ci.yml` 完全相同的 `devenv check` 与 `nix flake check`，并校验 tag
   版本与 workspace 版本一致，防止把未通过检查的提交发布出去；
2. **构建**：4 个目标平台的 release 二进制（均为原生 runner，无交叉编译）：

   | target | runner |
   | --- | --- |
   | `x86_64-unknown-linux-gnu` | `ubuntu-latest` |
   | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` |
   | `x86_64-apple-darwin` | `macos-13` |
   | `aarch64-apple-darwin` | `macos-latest` |

3. **发布**：`git-cliff --current` 生成该版本的 Release notes，将 tar.gz 与 SHA256
   校验和上传至 GitHub Release。

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

- **release-patch.yml 在 prepare/check/nix/build 阶段失败**：`main` 与 tag 均未
  变动。修复问题后重新 Run workflow 即可；`release/vX.Y.Z` 分支会被覆盖。
- **release-patch.yml 在 release 阶段失败**：若 tag 与 Release 已被创建（部分产物
  上传失败等），重跑会在 prepare 阶段因「tag 已存在」中止。需先在 GitHub 删除
  Release 与远端 tag（`git push origin :vX.Y.Z`）后重跑。
- **release-patch.yml 在 finalize 阶段失败**（创建 PR 失败）：Release 已发布，
  但 bump 提交未进入 main。人工创建 PR 后合并即可：

  ```bash
  gh pr create --base main --head release/vX.Y.Z \
    --title "chore(release): vX.Y.Z"
  ```

  若发版期间 main 前进导致 PR 冲突，将 release 分支 rebase 到最新 main 后
  force push 即可（tag 指向 rebase 前的旧提交，不重打，属预期）。
  注意合并发版 PR 必须用 merge commit（见上文「关键行为」）。
- **tag 打错（手动流程）**：未推送时 `git tag -d vX.Y.Z` 后 `jj abandon` 掉
  release 提交重来；已推送则在 GitHub 删除 Release 与远端 tag
  （`git push origin :vX.Y.Z`），修复后用新版本号重新发布，**不要复用已推送的 tag**。
- **release.yml（手动流程）失败**：修复问题后删除远端 tag 与 Release，按上面
  流程重发；不要绕过门禁直接上传产物。
- **aarch64-linux runner 不可用**（私有仓库无 ARM runner 额度）：将构建矩阵中对应
  条目改为 `ubuntu-latest` + `cross` 交叉编译。

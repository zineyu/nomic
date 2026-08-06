# 版本发布流程

nomic 的发布以 **git tag** 为唯一触发点：本地用 devenv `release` 脚本完成版本
bump、CHANGELOG 生成与打 tag，推送 tag 后由
[release.yml](../.github/workflows/release.yml) 自动完成检查、多平台构建与
GitHub Release 发布。

## 前置条件

- 在 devenv shell 中操作（`devenv shell` 或 direnv 自动进入），`release` 脚本、
  `cargo set-version`、`git-cliff` 均由 devenv 提供，版本锁定在 `devenv.lock`。
- commit message 遵循 Conventional Commits（`feat:` / `fix:` / `docs:` …），
  CHANGELOG 由 git-cliff 据此生成，分组规则见 [cliff.toml](../cliff.toml)。

## 发布步骤

```bash
# 1. 确保在 main 最新提交上、工作副本干净
jj new main

# 2. 执行发布脚本（以 0.2.0 为例）
release 0.2.0
```

脚本会依次执行，任一步失败即中止：

1. **前置校验**：版本号合法性、tag 未占用、工作副本为空、基于 `main` 最新提交；
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

## CI 自动完成的部分

tag 推送触发 `.github/workflows/release.yml`：

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

## 首次发布

仓库在引入本流程前没有任何 tag，git-cliff 会把**全部历史**归入第一个发布的
tag。两种处理方式：

- 直接发布 v0.1.0：全部历史进入 `## [0.1.0]` 段落，与预生成的基线一致；
- 发布更高版本（如 v0.2.0）：先在合适的历史提交补打 v0.1.0 tag，使首个
  Release 只含增量：

  ```bash
  git tag -a v0.1.0 <历史提交> -m v0.1.0
  git push origin v0.1.0   # 注意：这也会触发 release workflow
  ```

## 预告下一个版本的内容

```bash
git cliff --unreleased   # 预览上个 tag 以来将写入 CHANGELOG 的条目
```

## 故障处理

- **tag 打错**：未推送时 `git tag -d vX.Y.Z` 后 `jj abandon` 掉 release 提交重来；
  已推送则在 GitHub 删除 Release 与远端 tag（`git push origin :vX.Y.Z`），修复后
  用新版本号重新发布，**不要复用已推送的 tag**。
- **release workflow 失败**：修复问题后删除远端 tag 与 Release，按上面流程重发；
  不要绕过门禁直接上传产物。
- **aarch64-linux runner 不可用**（私有仓库无 ARM runner 额度）：将构建矩阵中对应
  条目改为 `ubuntu-latest` + `cross` 交叉编译。

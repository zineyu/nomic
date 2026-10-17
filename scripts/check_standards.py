# 仓库结构检查：crate 布局、层序依赖、文件命名、模块文档、crate 配置继承。
#
# 与其他 check 步骤一致：任一规则不满足即失败，不提供豁免。
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = ROOT / "crates"

# 层序：依赖只能指向更低层。新增层时在此登记。
LAYERS = {"core": 0, "ai": 1, "server": 2, "tui": 3}

FILE_NAME = re.compile(r"^[a-z][a-z0-9_]*\.rs$")
TEST_ATTRS = ("#[test]", "#[tokio::test]")

# 第三方依赖隔离：适配层独占，禁止其他 crate 直接使用。
RESTRICTED = {
    "rig-core": {"nomic-ai"},
    "rusqlite": {"nomic-server"},
}

failures: list[str] = []


def report(message: str) -> None:
    failures.append(message)
    print(f"❌ {message}", file=sys.stderr)


def load_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def check_workspace() -> dict[str, dict]:
    workspace = load_toml(ROOT / "Cargo.toml")
    members = sorted(workspace["workspace"]["members"])
    found = sorted(str(path.parent.relative_to(ROOT)) for path in CRATES.glob("*/*/Cargo.toml"))
    if members != found:
        report(f"workspace members 与 crates/ 目录不一致：声明 {members}，实际 {found}")

    packages: dict[str, dict] = {}
    for member in members:
        manifest = load_toml(ROOT / member / "Cargo.toml")
        package = manifest["package"]
        name = package["name"]
        parts = Path(member).parts
        if len(parts) != 3 or parts[1] not in LAYERS:
            report(f"{member}: crate 必须位于 crates/<layer>/，layer 只能是 {sorted(LAYERS)}")
            continue
        layer = parts[1]
        if name != parts[2] or not name.startswith("nomic-"):
            report(f"{member}: 包名必须等于目录名并以 nomic- 开头（当前 {name}）")
        for key in ("version", "edition", "rust-version", "license", "repository"):
            value = package.get(key)
            if not isinstance(value, dict) or value.get("workspace") is not True:
                report(f"{member}: package.{key} 必须继承 workspace")
        if manifest.get("lints", {}).get("workspace") is not True:
            report(f"{member}: 缺少 [lints] workspace = true")
        packages[name] = {"layer": layer, "dir": ROOT / member}
    return packages


def check_dependencies(packages: dict[str, dict]) -> None:
    for name, info in packages.items():
        manifest = load_toml(info["dir"] / "Cargo.toml")
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            for dep, spec in manifest.get(table, {}).items():
                target = spec.get("package", dep) if isinstance(spec, dict) else dep
                if target in RESTRICTED and name not in RESTRICTED[target]:
                    report(f"{name}: 第三方依赖 {target} 只允许出现在 {sorted(RESTRICTED[target])}")
                if target not in packages:
                    continue
                if LAYERS[packages[target]["layer"]] >= LAYERS[info["layer"]]:
                    report(
                        f"{name}: {table} 中的 {target} 违反层序"
                        f"（{info['layer']} 不得依赖 {packages[target]['layer']}）"
                    )


def check_files() -> None:
    for path in sorted(CRATES.rglob("*.rs")):
        rel = path.relative_to(ROOT)
        if not FILE_NAME.match(path.name):
            report(f"{rel}: 文件名必须是小写 snake_case")
        if "src" in rel.parts:
            first = path.read_text(encoding="utf-8").splitlines()[:1]
            if not first or not first[0].startswith("//!"):
                report(f"{rel}: 源文件必须以 //! 模块文档开头")
        if "tests" in rel.parts and "common" not in rel.parts:
            body = path.read_text(encoding="utf-8")
            if not any(attr in body for attr in TEST_ATTRS):
                report(f"{rel}: 集成测试文件必须包含测试用例")


def check_entry_points(packages: dict[str, dict]) -> None:
    for name, info in packages.items():
        if not (info["dir"] / "src" / "lib.rs").is_file() and not (
            info["dir"] / "src" / "main.rs"
        ).is_file():
            report(f"{name}: 缺少 src/lib.rs 或 src/main.rs")


def main() -> int:
    packages = check_workspace()
    check_dependencies(packages)
    check_entry_points(packages)
    check_files()
    if failures:
        print("", file=sys.stderr)
        print(f"结构检查未通过（{len(failures)} 项）", file=sys.stderr)
        return 1
    print(f"✅ 结构检查通过（{len(packages)} 个 crate）")
    return 0


if __name__ == "__main__":
    sys.exit(main())

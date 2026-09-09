# lucide_icons（vendored）

来源：pub.dev `lucide_icons` 0.257.0（MIT，见 LICENSE）。vendored 原因：
Flutter 3.44 起 `IconData` 变为 `final class`，官方包通过 `extends IconData`
实现已无法编译，且上游仓库已移除 Flutter 包、无修复版本。本拷贝将生成的
`LucideIconData(...)` 构造全部改为 `const IconData(..., fontFamily: 'Lucide',
fontPackage: 'lucide_icons')`，并剔除了 tool/test/example。升级 Flutter 后
若官方包恢复维护，可切回 hosted 版本。

---

# lucide_icons

Lucide Icons ([lucide.dev](https://lucide.dev)) for Flutter. Visit the website for the full list of icons

## Example
```dart
Icon(LucideIcons.activity);
```

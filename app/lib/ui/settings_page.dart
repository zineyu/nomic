/// 设置页：常规标量 + providers + 模型覆盖（ADR-0039；读写经 WS 事件协议，
/// 与 `nomic config` / TUI `/config` 同一口径）。写操作成功后服务端广播
/// `settings_changed`，控制层自动重新拉取快照，本页经 ListenableBuilder 刷新。
library;

import 'package:flutter/material.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../protocol/models.dart';
import '../theme.dart';
import 'animations.dart';
import 'settings_dialogs.dart';

class SettingsPage extends StatefulWidget {
  const SettingsPage({super.key, required this.controller});

  final AppController controller;

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  final _appendSystem = TextEditingController();
  final _promptPath = TextEditingController();
  final _aliasName = TextEditingController();
  final _aliasTarget = TextEditingController();

  /// 追加系统提示词文本框是否已按快照初始化（快照异步到达，不覆盖编辑中内容）。
  bool _appendInitialized = false;

  AppController get controller => widget.controller;

  @override
  void initState() {
    super.initState();
    // 跟踪编辑态：内容偏离快照值时展示「保存」按钮
    _appendSystem.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _appendSystem.dispose();
    _promptPath.dispose();
    _aliasName.dispose();
    _aliasTarget.dispose();
    super.dispose();
  }

  /// 执行设置写操作；失败消息经 SnackBar 展示，成功给轻量确认。
  Future<void> _report(Future<String?> action) async {
    final message = await action;
    if (!mounted) return;
    final messenger = ScaffoldMessenger.of(context);
    if (message != null) {
      messenger.showSnackBar(SnackBar(content: Text(message)));
    } else {
      messenger.showSnackBar(
        const SnackBar(content: Text('已保存'), duration: Duration(seconds: 1)),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return ListenableBuilder(
      listenable: controller,
      builder: (context, _) {
        final snapshot = controller.settings;
        if (snapshot != null && !_appendInitialized) {
          _appendSystem.text = snapshot.appendSystem;
          _appendInitialized = true;
        }
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _Header(onClose: controller.closeSettings),
            Expanded(
              child: snapshot == null
                  ? Center(
                      child: Text(
                        controller.error ?? '加载中…',
                        style: AppText.s(tokens.secondary),
                      ),
                    )
                  : Center(
                      child: ConstrainedBox(
                        constraints: const BoxConstraints(
                          maxWidth: maxPageWidth,
                        ),
                        child: ListView(
                          padding: const EdgeInsets.all(Spacing.lg),
                          children: [
                            _sectionTitle(tokens, '常规'),
                            _compactionCard(tokens, snapshot),
                            const SizedBox(height: Spacing.md),
                            _appendSystemCard(tokens, snapshot),
                            const SizedBox(height: Spacing.md),
                            _promptsCard(tokens, snapshot),
                            const SizedBox(height: Spacing.md),
                            _aliasesCard(tokens, snapshot),
                            const SizedBox(height: Spacing.xl),
                            _sectionTitle(
                              tokens,
                              'Providers',
                              action: _AddButton(
                                label: '添加 provider',
                                onPressed: () =>
                                    ProviderDialog.show(context, controller),
                              ),
                            ),
                            if (snapshot.providers.isEmpty)
                              _emptyHint(tokens, '没有 provider 定义。')
                            else
                              for (final provider in snapshot.providers)
                                _providerTile(tokens, provider),
                            const SizedBox(height: Spacing.xl),
                            _sectionTitle(
                              tokens,
                              '模型覆盖',
                              action: _AddButton(
                                label: '添加覆盖',
                                onPressed: snapshot.providers.isEmpty
                                    ? null
                                    : () => ModelSpecDialog.show(
                                        context,
                                        controller,
                                        snapshot.providers,
                                      ),
                              ),
                            ),
                            if (snapshot.modelSpecs.isEmpty)
                              _emptyHint(tokens, '没有模型覆盖。')
                            else
                              for (final row in snapshot.modelSpecs)
                                _modelSpecTile(tokens, row),
                          ],
                        ),
                      ),
                    ),
            ),
          ],
        );
      },
    );
  }

  // ── 区块骨架 ───────────────────────────────────────────────────────────

  Widget _sectionTitle(NomicTokens tokens, String title, {Widget? action}) {
    return Padding(
      padding: const EdgeInsets.only(bottom: Spacing.sm),
      child: Row(
        children: [
          Text(
            title,
            style: AppText.s(
              tokens.foreground,
            ).copyWith(fontWeight: FontWeight.w600),
          ),
          const Spacer(),
          ?action,
        ],
      ),
    );
  }

  Widget _emptyHint(NomicTokens tokens, String text) => Padding(
    padding: const EdgeInsets.only(bottom: Spacing.sm),
    child: Text(text, style: AppText.xs(tokens.secondary)),
  );

  Widget _card(NomicTokens tokens, List<Widget> children) => Container(
    width: double.infinity,
    decoration: BoxDecoration(
      color: tokens.card,
      borderRadius: BorderRadius.circular(Radii.xxl),
      border: Border.all(color: tokens.border),
    ),
    padding: const EdgeInsets.all(Spacing.md),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: children,
    ),
  );

  Widget _cardTitle(NomicTokens tokens, String title, String caption) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Text(title, style: AppText.xs(tokens.foreground)),
      Text(caption, style: AppText.xxs(tokens.secondary)),
    ],
  );

  // ── 常规 ───────────────────────────────────────────────────────────────

  Widget _compactionCard(NomicTokens tokens, SettingsSnapshot snapshot) {
    final isSet = snapshot.isSet(SettingKeys.compactionEnabled);
    return _card(tokens, [
      Row(
        children: [
          Expanded(
            child: _cardTitle(
              tokens,
              '自动压缩上下文',
              '接近上下文上限时自动压缩历史'
                  '（compaction.enabled${isSet ? '' : ' · 默认开启'}）',
            ),
          ),
          if (isSet)
            IconButton(
              icon: const Icon(LucideIcons.rotateCcw, size: 14),
              tooltip: '恢复默认',
              visualDensity: VisualDensity.compact,
              onPressed: () => _report(
                controller.unsetScalar(SettingKeys.compactionEnabled),
              ),
            ),
          Switch(
            value: snapshot.compactionEnabled,
            onChanged: (value) => _report(
              controller.setScalar(SettingKeys.compactionEnabled, value),
            ),
          ),
        ],
      ),
    ]);
  }

  Widget _appendSystemCard(NomicTokens tokens, SettingsSnapshot snapshot) {
    final dirty = _appendSystem.text != snapshot.appendSystem;
    final isSet = snapshot.isSet(SettingKeys.appendSystem);
    return _card(tokens, [
      _cardTitle(tokens, '追加系统提示词', '附加到系统提示词末尾（append_system）'),
      const SizedBox(height: Spacing.sm),
      TextField(
        controller: _appendSystem,
        minLines: 2,
        maxLines: 4,
        decoration: const InputDecoration(hintText: '如：总是用中文回复…'),
      ),
      // 保存/恢复默认操作行：随编辑态收展（高度 + 透明度，不硬跳）
      AnimatedReveal(
        visible: dirty || isSet,
        duration: AppMotion.fast,
        child: Padding(
          padding: const EdgeInsets.only(top: Spacing.sm),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              if (isSet)
                TextButton(
                  onPressed: () {
                    _appendSystem.clear();
                    _report(controller.unsetScalar(SettingKeys.appendSystem));
                  },
                  child: const Text('恢复默认'),
                ),
              if (dirty)
                FilledButton(
                  onPressed: () => _report(
                    controller.setScalar(
                      SettingKeys.appendSystem,
                      _appendSystem.text,
                    ),
                  ),
                  child: const Text('保存'),
                ),
            ],
          ),
        ),
      ),
    ]);
  }

  Widget _promptsCard(NomicTokens tokens, SettingsSnapshot snapshot) {
    final paths = snapshot.promptPaths;
    return _card(tokens, [
      _cardTitle(tokens, 'Prompt 模板路径', '额外的模板文件或目录（prompts）'),
      for (final path in paths)
        Row(
          children: [
            Expanded(
              child: Text(
                path,
                overflow: TextOverflow.ellipsis,
                style: AppText.xs(tokens.foreground),
              ),
            ),
            IconButton(
              icon: const Icon(LucideIcons.x, size: 14),
              tooltip: '移除',
              visualDensity: VisualDensity.compact,
              onPressed: () => _report(
                controller.setScalar(
                  SettingKeys.prompts,
                  paths.where((p) => p != path).toList(),
                ),
              ),
            ),
          ],
        ),
      const SizedBox(height: Spacing.sm),
      Row(
        children: [
          Expanded(
            child: TextField(
              controller: _promptPath,
              decoration: const InputDecoration(hintText: '添加路径…'),
              onSubmitted: (_) => _addPromptPath(paths),
            ),
          ),
          IconButton(
            icon: const Icon(LucideIcons.plus, size: 16),
            tooltip: '添加',
            onPressed: () => _addPromptPath(paths),
          ),
        ],
      ),
    ]);
  }

  void _addPromptPath(List<String> paths) {
    final path = _promptPath.text.trim();
    if (path.isEmpty || paths.contains(path)) return;
    _promptPath.clear();
    _report(controller.setScalar(SettingKeys.prompts, [...paths, path]));
  }

  Widget _aliasesCard(NomicTokens tokens, SettingsSnapshot snapshot) {
    final aliases = snapshot.modelAliases;
    return _card(tokens, [
      _cardTitle(
        tokens,
        '模型别名',
        '别名 → <provider>/<模型id>（model_aliases；子 agent 模型选择用）',
      ),
      for (final entry in aliases.entries)
        Row(
          children: [
            Text(entry.key, style: AppText.xs(tokens.foreground)),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: Spacing.sm),
              child: Icon(
                LucideIcons.arrowRight,
                size: 12,
                color: tokens.secondary,
              ),
            ),
            Expanded(
              child: Text(
                entry.value,
                overflow: TextOverflow.ellipsis,
                style: AppText.xs(tokens.foreground),
              ),
            ),
            IconButton(
              icon: const Icon(LucideIcons.x, size: 14),
              tooltip: '移除',
              visualDensity: VisualDensity.compact,
              onPressed: () => _report(
                controller.setScalar(
                  SettingKeys.modelAliases,
                  {...aliases}..remove(entry.key),
                ),
              ),
            ),
          ],
        ),
      const SizedBox(height: Spacing.sm),
      Row(
        children: [
          Expanded(
            child: TextField(
              controller: _aliasName,
              decoration: const InputDecoration(hintText: '别名（如 smart）'),
            ),
          ),
          const SizedBox(width: Spacing.sm),
          Expanded(
            flex: 2,
            child: TextField(
              controller: _aliasTarget,
              decoration: const InputDecoration(hintText: 'provider/模型id'),
              onSubmitted: (_) => _addAlias(aliases),
            ),
          ),
          IconButton(
            icon: const Icon(LucideIcons.plus, size: 16),
            tooltip: '添加',
            onPressed: () => _addAlias(aliases),
          ),
        ],
      ),
    ]);
  }

  void _addAlias(Map<String, String> aliases) {
    final name = _aliasName.text.trim();
    final target = _aliasTarget.text.trim();
    if (name.isEmpty || target.isEmpty) return;
    _aliasName.clear();
    _aliasTarget.clear();
    _report(
      controller.setScalar(SettingKeys.modelAliases, {
        ...aliases,
        name: target,
      }),
    );
  }

  // ── providers / 模型覆盖 ───────────────────────────────────────────────

  Widget _providerTile(NomicTokens tokens, ProviderView provider) {
    final subtitle = [
      provider.api ?? '按名推断',
      ?provider.baseUrl,
      'api_key ${provider.hasApiKey ? '已设置' : '未设置'}',
    ].join(' · ');
    return _card(tokens, [
      Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(provider.name, style: AppText.xs(tokens.foreground)),
                Text(
                  subtitle,
                  overflow: TextOverflow.ellipsis,
                  style: AppText.xxs(tokens.secondary),
                ),
              ],
            ),
          ),
          IconButton(
            icon: const Icon(LucideIcons.pencil, size: 14),
            tooltip: '编辑',
            visualDensity: VisualDensity.compact,
            onPressed: () => ProviderDialog.show(context, controller, provider),
          ),
          IconButton(
            icon: Icon(LucideIcons.trash2, size: 14, color: tokens.error),
            tooltip: '删除（其模型覆盖一并清除）',
            visualDensity: VisualDensity.compact,
            onPressed: () => _confirmDelete(
              title: '删除 provider ${provider.name}？',
              body: '其模型覆盖将一并清除。',
              onConfirm: () => controller.deleteProvider(provider.name),
            ),
          ),
        ],
      ),
    ]);
  }

  Widget _modelSpecTile(NomicTokens tokens, ModelSpecRow row) {
    return _card(tokens, [
      Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(row.spec, style: AppText.xs(tokens.foreground)),
                Text(
                  row.summary.isEmpty ? '（空覆盖）' : row.summary,
                  overflow: TextOverflow.ellipsis,
                  style: AppText.xxs(tokens.secondary),
                ),
              ],
            ),
          ),
          IconButton(
            icon: const Icon(LucideIcons.pencil, size: 14),
            tooltip: '编辑',
            visualDensity: VisualDensity.compact,
            onPressed: () => ModelSpecDialog.show(
              context,
              controller,
              controller.settings?.providers ?? const [],
              row,
            ),
          ),
          IconButton(
            icon: Icon(LucideIcons.trash2, size: 14, color: tokens.error),
            tooltip: '删除',
            visualDensity: VisualDensity.compact,
            onPressed: () => _confirmDelete(
              title: '删除模型覆盖 ${row.spec}？',
              body: '恢复 models.dev / 中性兜底解析。',
              onConfirm: () =>
                  controller.deleteModelSpec(row.provider, row.modelId),
            ),
          ),
        ],
      ),
    ]);
  }

  void _confirmDelete({
    required String title,
    required String body,
    required Future<String?> Function() onConfirm,
  }) {
    final tokens = tokensOf(context);
    showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(title, style: AppText.base(null)),
        content: Text(body),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            style: FilledButton.styleFrom(
              backgroundColor: tokens.error,
              foregroundColor: tokens.primaryForeground,
            ),
            onPressed: () {
              Navigator.of(context).pop();
              _report(onConfirm());
            },
            child: const Text('删除'),
          ),
        ],
      ),
    );
  }
}

/// 页头：标题 + 关闭按钮（返回聊天/启动页）。
class _Header extends StatelessWidget {
  const _Header({required this.onClose});

  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Container(
      padding: const EdgeInsets.symmetric(
        horizontal: Spacing.md,
        vertical: Spacing.sm,
      ),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: tokens.border)),
      ),
      child: Row(
        children: [
          Text(
            '设置',
            style: AppText.s(
              tokens.foreground,
            ).copyWith(fontWeight: FontWeight.w600),
          ),
          const Spacer(),
          IconButton(
            icon: const Icon(LucideIcons.x, size: 16),
            tooltip: '关闭',
            visualDensity: VisualDensity.compact,
            onPressed: onClose,
          ),
        ],
      ),
    );
  }
}

/// 区块标题右侧的「添加」幽灵按钮（图标 + 文字，ghost 样式）。
class _AddButton extends StatelessWidget {
  const _AddButton({required this.label, required this.onPressed});

  final String label;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) {
    return TextButton.icon(
      onPressed: onPressed,
      icon: const Icon(LucideIcons.plus, size: 14),
      label: Text(label, style: AppText.xs(null)),
    );
  }
}

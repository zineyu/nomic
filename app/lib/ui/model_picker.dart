/// 模型选择器：跨 provider 候选列表（`list_models` 查询 + `switch_model` 命令；
/// 选择结果由服务端落库，与 TUI `/models` 同一口径）。支持搜索过滤与加载态。
library;

import 'package:flutter/material.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../theme.dart';

class ModelPicker extends StatefulWidget {
  const ModelPicker({super.key, required this.controller});

  final AppController controller;

  static Future<void> show(BuildContext context, AppController controller) {
    // 打开时拉取候选列表（异步回填，Dialog 经 ListenableBuilder 自动刷新）
    // ignore: discarded_futures
    controller.loadModels();
    return showDialog<void>(
      context: context,
      builder: (context) => ModelPicker(controller: controller),
    );
  }

  @override
  State<ModelPicker> createState() => _ModelPickerState();
}

class _ModelPickerState extends State<ModelPicker> {
  final _searchController = TextEditingController();

  @override
  void dispose() {
    _searchController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return ListenableBuilder(
      listenable: widget.controller,
      builder: (context, _) {
        final query = _searchController.text.trim().toLowerCase();
        final all = widget.controller.modelCandidates;
        final candidates = query.isEmpty
            ? all
            : all
                  .where(
                    (c) =>
                        c.name.toLowerCase().contains(query) ||
                        c.spec.toLowerCase().contains(query) ||
                        c.provider.toLowerCase().contains(query),
                  )
                  .toList();
        final current = widget.controller.model;
        return AlertDialog(
          title: Text('选择模型', style: AppText.body(null)),
          content: SizedBox(
            width: 420,
            height: 480,
            child: Column(
              children: [
                TextField(
                  controller: _searchController,
                  autofocus: true,
                  style: AppText.ui(tokens.foreground),
                  decoration: const InputDecoration(
                    hintText: '搜索模型…',
                    isDense: true,
                    prefixIcon: Icon(LucideIcons.search, size: 14),
                    prefixIconConstraints: BoxConstraints(
                      minWidth: 32,
                      minHeight: 24,
                    ),
                  ),
                  onChanged: (_) => setState(() {}),
                ),
                const SizedBox(height: Spacing.sm),
                Expanded(
                  child: widget.controller.modelsLoading
                      ? Center(
                          child: SizedBox(
                            width: 16,
                            height: 16,
                            child: CircularProgressIndicator(
                              strokeWidth: 2,
                              color: tokens.mutedForeground,
                            ),
                          ),
                        )
                      : all.isEmpty
                      ? Center(
                          child: Text(
                            '无候选模型（在设置中配置 provider）',
                            style: AppText.ui(tokens.mutedForeground),
                          ),
                        )
                      : candidates.isEmpty
                      ? Center(
                          child: Text(
                            '无匹配模型',
                            style: AppText.ui(tokens.mutedForeground),
                          ),
                        )
                      : ListView(
                          children: [
                            for (final choice in candidates)
                              ListTile(
                                dense: true,
                                leading: Icon(
                                  choice.provider == current.provider &&
                                          choice.id == current.id
                                      ? LucideIcons.check
                                      : LucideIcons.cpu,
                                  size: 14,
                                  color: tokens.mutedForeground,
                                ),
                                title: Text(
                                  choice.name,
                                  style: AppText.ui(null),
                                ),
                                subtitle: Text(
                                  '${choice.spec} · ${choice.contextWindow} ctx'
                                  '${choice.reasoning ? ' · reasoning' : ''}',
                                  style: AppText.caption(
                                    tokens.mutedForeground,
                                  ),
                                ),
                                onTap: () {
                                  widget.controller.switchModel(choice.spec);
                                  Navigator.of(context).pop();
                                },
                              ),
                          ],
                        ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

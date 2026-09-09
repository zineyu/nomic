/// 侧栏：work 列表（按 project 分组）+ 新建入口。
///
/// work 是侧栏一等入口（ADR-0044）：点击打开其主 session。
library;

import 'package:flutter/material.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../protocol/models.dart';
import '../theme.dart';

class Sidebar extends StatelessWidget {
  const Sidebar({super.key, required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    // 按 project 分组（保持 works 原有顺序：服务端按末条消息时间降序）
    final byProject = <String, List<WorkSummary>>{};
    for (final work in controller.works) {
      byProject.putIfAbsent(work.project, () => []).add(work);
    }

    return Container(
      width: 260,
      color: tokens.sidebar,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.all(Spacing.md),
            child: Row(
              children: [
                Text(
                  'Nomic',
                  style: TextStyle(
                    fontSize: 14,
                    fontWeight: FontWeight.w600,
                    color: tokens.foreground,
                  ),
                ),
                const Spacer(),
                IconButton(
                  icon: const Icon(LucideIcons.plus, size: 16),
                  tooltip: '新 work（选择 project）',
                  visualDensity: VisualDensity.compact,
                  onPressed: controller.closeSession,
                ),
              ],
            ),
          ),
          Expanded(
            child: ListView(
              padding: const EdgeInsets.symmetric(horizontal: Spacing.sm),
              children: [
                for (final entry in byProject.entries) ...[
                  Padding(
                    padding: const EdgeInsets.fromLTRB(
                      Spacing.sm,
                      Spacing.md,
                      Spacing.sm,
                      Spacing.sm,
                    ),
                    child: Text(
                      entry.key,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 12,
                        color: tokens.mutedForeground,
                      ),
                    ),
                  ),
                  for (final work in entry.value)
                    _WorkTile(
                      work: work,
                      onTap: () => controller.openSession(work.mainSessionId),
                    ),
                ],
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _WorkTile extends StatelessWidget {
  const _WorkTile({required this.work, required this.onTap});

  final WorkSummary work;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 2),
      child: Material(
        color: Colors.transparent,
        borderRadius: BorderRadius.circular(Radii.md),
        child: InkWell(
          borderRadius: BorderRadius.circular(Radii.md),
          hoverColor: tokens.sidebarAccent,
          onTap: onTap,
          child: Padding(
            padding: const EdgeInsets.symmetric(
              horizontal: Spacing.sm,
              vertical: Spacing.sm,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  work.displayTitle,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 13, color: tokens.foreground),
                ),
                Text(
                  '${work.messageCount} 条消息 · ${work.sessionCount} 个会话',
                  style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

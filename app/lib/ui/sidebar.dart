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
                      selected: controller.sessionId == work.mainSessionId,
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
  const _WorkTile({
    required this.work,
    required this.selected,
    required this.onTap,
  });

  final WorkSummary work;

  /// 当前打开的 work（Codex 侧栏同款：neutral accent 填充 + 字重标记
  /// 当前项，不用彩色 pill）。
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 2),
      child: Material(
        color: selected ? tokens.sidebarAccent : Colors.transparent,
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
                  style: TextStyle(
                    fontSize: 13,
                    fontWeight: selected ? FontWeight.w500 : FontWeight.w400,
                    color: tokens.foreground,
                  ),
                ),
                Text(
                  _workSubtitle(work),
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

/// 侧栏副标题：相对时间（Codex 同款）+ 消息数。
String _workSubtitle(WorkSummary work) {
  final time = _relativeTime(work.lastMessageAt);
  if (work.messageCount == 0) return '尚无消息';
  return time == null
      ? '${work.messageCount} 条消息'
      : '$time · ${work.messageCount} 条消息';
}

/// Unix 毫秒 → 相对时间（刚刚 / n 分钟前 / n 小时前 / 昨天 / n 天前）。
String? _relativeTime(int? millis) {
  if (millis == null) return null;
  final then = DateTime.fromMillisecondsSinceEpoch(millis);
  final now = DateTime.now();
  final diff = now.difference(then);
  if (diff.inMinutes < 1) return '刚刚';
  if (diff.inHours < 1) return '${diff.inMinutes} 分钟前';
  if (diff.inHours < 24 && then.day == now.day) return '${diff.inHours} 小时前';
  final days = diff.inDays;
  if (days == 1) return '昨天';
  if (days < 30) return '$days 天前';
  return '${then.month}/${then.day}';
}

/// 侧栏：work 列表（按 project 分组）+ 新建入口。
///
/// work 是侧栏一等入口（ADR-0044）：点击打开其主 session。
library;

import 'package:flutter/material.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../protocol/models.dart';
import '../theme.dart';

class Sidebar extends StatefulWidget {
  const Sidebar({super.key, required this.controller});

  final AppController controller;

  @override
  State<Sidebar> createState() => _SidebarState();
}

class _SidebarState extends State<Sidebar> {
  final _searchController = TextEditingController();
  final _listController = ScrollController();

  @override
  void dispose() {
    _searchController.dispose();
    _listController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final controller = widget.controller;
    final query = _searchController.text.trim().toLowerCase();
    final works = query.isEmpty
        ? controller.works
        : controller.works
              .where(
                (w) =>
                    w.displayTitle.toLowerCase().contains(query) ||
                    w.project.toLowerCase().contains(query),
              )
              .toList();
    // 按 project 分组（保持 works 原有顺序：服务端按末条消息时间降序）
    final byProject = <String, List<WorkSummary>>{};
    for (final work in works) {
      byProject.putIfAbsent(work.project, () => []).add(work);
    }

    return Container(
      width: 260,
      color: tokens.sidebar,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // 品牌行
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Spacing.md,
              Spacing.md,
              Spacing.md,
              Spacing.sm,
            ),
            child: Text(
              'Nomic',
              style: AppText.bodySm(
                tokens.foreground,
              ).copyWith(fontWeight: FontWeight.w700),
            ),
          ),
          // 主导航：新建任务
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: Spacing.sm),
            child: _NavTile(
              icon: LucideIcons.plus,
              label: '新建任务',
              onTap: controller.closeSession,
            ),
          ),
          const SizedBox(height: Spacing.sm),
          // 搜索（标题 / project 路径过滤）
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Spacing.md,
              0,
              Spacing.md,
              Spacing.sm,
            ),
            child: TextField(
              controller: _searchController,
              style: AppText.ui(tokens.foreground),
              decoration: InputDecoration(
                hintText: '搜索会话…',
                isDense: true,
                prefixIcon: const Icon(LucideIcons.search, size: 14),
                prefixIconConstraints: const BoxConstraints(
                  minWidth: 32,
                  minHeight: 24,
                ),
                contentPadding: const EdgeInsets.symmetric(
                  vertical: Spacing.sm,
                ),
              ),
              onChanged: (_) => setState(() {}),
            ),
          ),
          Expanded(
            child: works.isEmpty
                ? Center(
                    child: Text(
                      controller.works.isEmpty ? '暂无 work' : '无匹配会话',
                      style: AppText.ui(tokens.mutedForeground),
                    ),
                  )
                : Scrollbar(
                    controller: _listController,
                    thumbVisibility: true,
                    child: ListView(
                      controller: _listController,
                      padding: const EdgeInsets.symmetric(
                        horizontal: Spacing.sm,
                      ),
                      children: [
                        Padding(
                          padding: const EdgeInsets.fromLTRB(
                            Spacing.sm,
                            Spacing.sm,
                            Spacing.sm,
                            4,
                          ),
                          child: Text(
                            '项目',
                            style: AppText.caption(tokens.mutedForeground),
                          ),
                        ),
                        for (final entry in byProject.entries) ...[
                          Padding(
                            padding: const EdgeInsets.fromLTRB(
                              Spacing.sm,
                              Spacing.md,
                              Spacing.sm,
                              Spacing.sm,
                            ),
                            child: Row(
                              children: [
                                Icon(
                                  LucideIcons.folder,
                                  size: 14,
                                  color: tokens.mutedForeground,
                                ),
                                const SizedBox(width: Spacing.sm),
                                Expanded(
                                  child: Tooltip(
                                    message: entry.key,
                                    child: Text(
                                      _basename(entry.key),
                                      overflow: TextOverflow.ellipsis,
                                      style: AppText.ui(tokens.foreground),
                                    ),
                                  ),
                                ),
                              ],
                            ),
                          ),
                          for (final work in entry.value)
                            _WorkTile(
                              work: work,
                              controller: controller,
                              selected:
                                  controller.sessionId == work.mainSessionId,
                              onTap: () =>
                                  controller.openSession(work.mainSessionId),
                            ),
                        ],
                      ],
                    ),
                  ),
          ),
          // 底部：设置页入口
          Container(
            decoration: BoxDecoration(
              border: Border(top: BorderSide(color: tokens.sidebarBorder)),
            ),
            child: Material(
              color: controller.showingSettings
                  ? tokens.sidebarAccent
                  : Colors.transparent,
              child: InkWell(
                hoverColor: tokens.sidebarAccent,
                onTap: controller.openSettings,
                child: Padding(
                  padding: const EdgeInsets.all(Spacing.md),
                  child: Row(
                    children: [
                      Icon(
                        LucideIcons.settings,
                        size: 14,
                        color: tokens.mutedForeground,
                      ),
                      const SizedBox(width: Spacing.sm),
                      Text('设置', style: AppText.ui(tokens.foreground)),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// 主导航行（新建任务）：图标 + 标签，hover 底色 sidebarAccent。
class _NavTile extends StatelessWidget {
  const _NavTile({
    required this.icon,
    required this.label,
    required this.onTap,
  });

  final IconData icon;
  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Material(
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
          child: Row(
            children: [
              Icon(icon, size: 14, color: tokens.foreground),
              const SizedBox(width: Spacing.sm),
              Text(label, style: AppText.ui(tokens.foreground)),
            ],
          ),
        ),
      ),
    );
  }
}

class _WorkTile extends StatefulWidget {
  const _WorkTile({
    required this.work,
    required this.controller,
    required this.selected,
    required this.onTap,
  });

  final WorkSummary work;
  final AppController controller;

  /// 当前打开的 work（Codex 侧栏同款：neutral accent 填充 + 字重标记
  /// 当前项，不用彩色 pill）。
  final bool selected;
  final VoidCallback onTap;

  @override
  State<_WorkTile> createState() => _WorkTileState();
}

class _WorkTileState extends State<_WorkTile> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final selected = widget.selected;
    final work = widget.work;
    return Padding(
      padding: const EdgeInsets.only(bottom: 2),
      child: MouseRegion(
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: Material(
          color: selected ? tokens.sidebarAccent : Colors.transparent,
          borderRadius: BorderRadius.circular(Radii.md),
          child: InkWell(
            borderRadius: BorderRadius.circular(Radii.md),
            hoverColor: tokens.sidebarAccent,
            onTap: widget.onTap,
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Spacing.sm,
                vertical: Spacing.sm,
              ),
              child: Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          work.displayTitle,
                          overflow: TextOverflow.ellipsis,
                          style: AppText.ui(tokens.foreground).copyWith(
                            fontWeight: selected
                                ? FontWeight.w500
                                : FontWeight.w400,
                          ),
                        ),
                        Text(
                          _workSubtitle(work),
                          style: AppText.caption(tokens.mutedForeground),
                        ),
                      ],
                    ),
                  ),
                  // hover 时露出删除入口；否则当前会话带 signal 圆点
                  if (_hovered)
                    GestureDetector(
                      onTap: () => _confirmDelete(context),
                      child: Padding(
                        padding: const EdgeInsets.only(left: Spacing.sm),
                        child: Icon(
                          LucideIcons.trash2,
                          size: 14,
                          color: tokens.mutedForeground,
                        ),
                      ),
                    )
                  else if (selected)
                    Padding(
                      padding: const EdgeInsets.only(left: Spacing.sm),
                      child: Container(
                        width: 8,
                        height: 8,
                        decoration: BoxDecoration(
                          color: tokens.signal,
                          shape: BoxShape.circle,
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  void _confirmDelete(BuildContext context) {
    final tokens = tokensOf(context);
    final controller = widget.controller;
    showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('删除这个 work？', style: AppText.body(null)),
        content: Text('「${widget.work.displayTitle}」及其全部会话将被删除。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            style: FilledButton.styleFrom(
              backgroundColor: tokens.destructive,
              foregroundColor: tokens.primaryForeground,
            ),
            onPressed: () {
              Navigator.of(context).pop();
              controller.deleteWork(widget.work.id);
            },
            child: const Text('删除'),
          ),
        ],
      ),
    );
  }
}

/// project 路径末段（分组头显示用；完整路径走 tooltip）。
String _basename(String path) {
  final segments = path.split(RegExp(r'[/\\]')).where((s) => s.isNotEmpty);
  return segments.isEmpty ? path : segments.last;
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

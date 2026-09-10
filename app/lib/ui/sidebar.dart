/// 侧栏：品牌行 + 新建任务 + 搜索 + work 列表（按 project 分组，可折叠）。
///
/// work 是侧栏一等入口（ADR-0044）：点击打开其主 session。
/// 状态指示三态分离：选中 = 浅灰底 + 左侧 3px accent 条；未读 = accent 圆点；
/// 运行中 = spinner（运行/未读由 controller 从全局事件流推导）。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../protocol/models.dart';
import '../theme.dart';

class Sidebar extends StatefulWidget {
  const Sidebar({super.key, required this.controller, this.searchFocusNode});

  final AppController controller;

  /// 搜索框焦点（⌘K 由 HomePage 注入）。
  final FocusNode? searchFocusNode;

  @override
  State<Sidebar> createState() => _SidebarState();
}

class _SidebarState extends State<Sidebar> {
  final _searchController = TextEditingController();
  final _listController = ScrollController();
  final _listFocusNode = FocusNode();

  /// 折叠的 project 路径集合。
  final _collapsed = <String>{};

  /// 键盘导航的当前下标（相对「可见 work 扁平列表」）。
  int _focusedIndex = -1;

  @override
  void dispose() {
    _searchController.dispose();
    _listController.dispose();
    _listFocusNode.dispose();
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
    // 键盘导航用的可见 work 扁平列表（跳过折叠分组）
    final visible = <WorkSummary>[
      for (final entry in byProject.entries)
        if (!_collapsed.contains(entry.key)) ...entry.value,
    ];
    if (_focusedIndex >= visible.length) _focusedIndex = visible.length - 1;

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
          // 新建任务（浅色填充按钮；⌘N 快捷键在 HomePage）
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: Spacing.md),
            child: _NewTaskButton(onTap: controller.closeSession),
          ),
          const SizedBox(height: Spacing.sm),
          // 搜索（标题 / project 路径过滤；⌘K 聚焦）
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Spacing.md,
              0,
              Spacing.md,
              Spacing.sm,
            ),
            child: TextField(
              controller: _searchController,
              focusNode: widget.searchFocusNode,
              style: AppText.ui(tokens.foreground),
              decoration: InputDecoration(
                hintText: '搜索会话…',
                isDense: true,
                prefixIcon: const Icon(LucideIcons.search, size: 14),
                prefixIconConstraints: const BoxConstraints(
                  minWidth: 32,
                  minHeight: 32,
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
                : Focus(
                    focusNode: _listFocusNode,
                    onKeyEvent: (node, event) => _onListKey(event, visible),
                    child: Scrollbar(
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
                            _ProjectHeader(
                              path: entry.key,
                              collapsed: _collapsed.contains(entry.key),
                              onToggle: () => setState(() {
                                if (!_collapsed.remove(entry.key)) {
                                  _collapsed.add(entry.key);
                                }
                              }),
                            ),
                            if (!_collapsed.contains(entry.key))
                              for (final work in entry.value)
                                // 条目缩进与组头文本对齐（图标 14 + 间距 8）
                                Padding(
                                  padding: const EdgeInsets.only(left: 14),
                                  child: _WorkTile(
                                    work: work,
                                    controller: controller,
                                    selected:
                                        controller.sessionId ==
                                        work.mainSessionId,
                                    running: controller.runningSessions
                                        .contains(work.mainSessionId),
                                    unread:
                                        controller.unreadSessions.contains(
                                          work.mainSessionId,
                                        ) &&
                                        controller.sessionId !=
                                            work.mainSessionId,
                                    keyboardFocused:
                                        visible.indexOf(work) == _focusedIndex,
                                    onTap: () {
                                      _listFocusNode.requestFocus();
                                      controller.openSession(
                                        work.mainSessionId,
                                      );
                                    },
                                  ),
                                ),
                          ],
                        ],
                      ),
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

  /// 列表键盘导航：↑/↓ 移动焦点，Enter 打开。
  KeyEventResult _onListKey(KeyEvent event, List<WorkSummary> visible) {
    if (event is! KeyDownEvent || visible.isEmpty) {
      return KeyEventResult.ignored;
    }
    if (event.logicalKey == LogicalKeyboardKey.arrowDown) {
      setState(
        () => _focusedIndex = (_focusedIndex + 1).clamp(0, visible.length - 1),
      );
      return KeyEventResult.handled;
    }
    if (event.logicalKey == LogicalKeyboardKey.arrowUp) {
      setState(
        () => _focusedIndex = (_focusedIndex - 1).clamp(0, visible.length - 1),
      );
      return KeyEventResult.handled;
    }
    if (event.logicalKey == LogicalKeyboardKey.enter && _focusedIndex >= 0) {
      widget.controller.openSession(visible[_focusedIndex].mainSessionId);
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }
}

/// 新建任务按钮：浅色填充（surface），hover 加深，加号图标 + 32px 点击高度。
class _NewTaskButton extends StatefulWidget {
  const _NewTaskButton({required this.onTap});

  final VoidCallback onTap;

  @override
  State<_NewTaskButton> createState() => _NewTaskButtonState();
}

class _NewTaskButtonState extends State<_NewTaskButton> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: Material(
        color: _hovered ? tokens.sidebarAccent : tokens.secondary,
        borderRadius: BorderRadius.circular(Radii.lg),
        child: InkWell(
          borderRadius: BorderRadius.circular(Radii.lg),
          hoverColor: Colors.transparent,
          onTap: widget.onTap,
          child: SizedBox(
            height: 32,
            child: Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                Icon(LucideIcons.plus, size: 14, color: tokens.foreground),
                const SizedBox(width: 4),
                Text(
                  '新建任务',
                  style: AppText.ui(
                    tokens.foreground,
                  ).copyWith(fontWeight: FontWeight.w500),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// project 组头：chevron + 文件夹图标 + 名称，整行可点折叠/展开。
class _ProjectHeader extends StatelessWidget {
  const _ProjectHeader({
    required this.path,
    required this.collapsed,
    required this.onToggle,
  });

  final String path;
  final bool collapsed;
  final VoidCallback onToggle;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Tooltip(
      message: path,
      child: InkWell(
        borderRadius: BorderRadius.circular(Radii.md),
        hoverColor: tokens.sidebarAccent,
        onTap: onToggle,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(
            Spacing.sm,
            Spacing.sm,
            Spacing.sm,
            Spacing.sm,
          ),
          child: Row(
            children: [
              Icon(
                collapsed ? LucideIcons.chevronRight : LucideIcons.chevronDown,
                size: 12,
                color: tokens.tertiary,
              ),
              const SizedBox(width: 4),
              Icon(LucideIcons.folder, size: 14, color: tokens.mutedForeground),
              const SizedBox(width: Spacing.sm),
              Expanded(
                child: Text(
                  _basename(path),
                  overflow: TextOverflow.ellipsis,
                  style: AppText.ui(
                    tokens.foreground,
                  ).copyWith(fontWeight: FontWeight.w500),
                ),
              ),
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
    required this.running,
    required this.unread,
    required this.keyboardFocused,
    required this.onTap,
  });

  final WorkSummary work;
  final AppController controller;

  /// 当前打开的 work：浅灰底 + 左侧 3px accent 条 + 字重标记。
  final bool selected;

  /// 运行中（spinner）与未读（accent 圆点）分离；未读不对选中项展示。
  final bool running;
  final bool unread;
  final bool keyboardFocused;
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
        child: GestureDetector(
          onSecondaryTapUp: (details) => _showContextMenu(details, context),
          child: Material(
            color: selected
                ? tokens.sidebarAccent
                : widget.keyboardFocused
                ? tokens.secondary
                : Colors.transparent,
            borderRadius: BorderRadius.circular(Radii.lg),
            child: InkWell(
              borderRadius: BorderRadius.circular(Radii.lg),
              hoverColor: selected ? null : tokens.sidebarAccent,
              onTap: widget.onTap,
              child: Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: Spacing.sm,
                  vertical: Spacing.sm,
                ),
                child: Row(
                  children: [
                    // 选中强调条（3px accent；未选中占位保持对齐）
                    Container(
                      width: 3,
                      height: 18,
                      decoration: BoxDecoration(
                        color: selected ? tokens.accent : Colors.transparent,
                        borderRadius: BorderRadius.circular(Radii.full),
                      ),
                    ),
                    const SizedBox(width: Spacing.sm),
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
                    // 右侧状态位：hover 时让位给删除入口；运行中 spinner >
                    // 未读圆点 > 无
                    if (_hovered)
                      GestureDetector(
                        onTap: () => showDeleteWorkDialog(
                          context,
                          widget.controller,
                          widget.work,
                        ),
                        child: Padding(
                          padding: const EdgeInsets.only(left: Spacing.sm),
                          child: Icon(
                            LucideIcons.trash2,
                            size: 14,
                            color: tokens.mutedForeground,
                          ),
                        ),
                      )
                    else if (widget.running)
                      Padding(
                        padding: const EdgeInsets.only(left: Spacing.sm),
                        child: SizedBox(
                          width: 12,
                          height: 12,
                          child: CircularProgressIndicator(
                            strokeWidth: 2,
                            color: tokens.accent,
                          ),
                        ),
                      )
                    else if (widget.unread)
                      Padding(
                        padding: const EdgeInsets.only(left: Spacing.sm),
                        child: Container(
                          width: 8,
                          height: 8,
                          decoration: BoxDecoration(
                            color: tokens.accent,
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
      ),
    );
  }

  /// 右键菜单：重命名 / 删除（置顶与归档待服务端支持后开放）。
  void _showContextMenu(TapUpDetails details, BuildContext context) {
    final tokens = tokensOf(context);
    showMenu<String>(
      context: context,
      position: RelativeRect.fromLTRB(
        details.globalPosition.dx,
        details.globalPosition.dy,
        details.globalPosition.dx,
        details.globalPosition.dy,
      ),
      items: [
        PopupMenuItem(
          value: 'rename',
          height: 32,
          child: Text('重命名', style: AppText.ui(tokens.foreground)),
        ),
        PopupMenuItem(
          value: 'delete',
          height: 32,
          child: Text('删除', style: AppText.ui(tokens.destructive)),
        ),
      ],
    ).then((value) {
      if (!context.mounted) return;
      if (value == 'rename') {
        showRenameWorkDialog(context, widget.controller, widget.work);
      }
      if (value == 'delete') {
        showDeleteWorkDialog(context, widget.controller, widget.work);
      }
    });
  }
}

/// 重命名 work 对话框（侧栏右键菜单与聊天页上下文栏共用）。
void showRenameWorkDialog(
  BuildContext context,
  AppController app,
  WorkSummary work,
) {
  final tokens = tokensOf(context);
  final controller = TextEditingController(text: work.title ?? '');
  void submit(BuildContext context) {
    final title = controller.text.trim();
    Navigator.of(context).pop();
    if (title.isNotEmpty) app.renameWork(work.id, title);
  }

  showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text('重命名任务', style: AppText.body(null)),
      content: TextField(
        controller: controller,
        autofocus: true,
        decoration: const InputDecoration(hintText: '任务标题'),
        onSubmitted: (_) => submit(context),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          style: FilledButton.styleFrom(
            backgroundColor: tokens.primary,
            foregroundColor: tokens.primaryForeground,
          ),
          onPressed: () => submit(context),
          child: const Text('保存'),
        ),
      ],
    ),
  );
}

/// 删除 work 确认对话框（侧栏与聊天页上下文栏共用）。
void showDeleteWorkDialog(
  BuildContext context,
  AppController app,
  WorkSummary work,
) {
  final tokens = tokensOf(context);
  showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text('删除这个 work？', style: AppText.body(null)),
      content: Text('「${work.displayTitle}」及其全部会话将被删除。'),
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
            app.deleteWork(work.id);
          },
          child: const Text('删除'),
        ),
      ],
    ),
  );
}

/// project 路径末段（分组头显示用；完整路径走 tooltip）。
String _basename(String path) {
  final segments = path.split(RegExp(r'[/\\]')).where((s) => s.isNotEmpty);
  return segments.isEmpty ? path : segments.last;
}

/// 侧栏副标题：相对时间 + 消息数。
String _workSubtitle(WorkSummary work) {
  final time = _relativeTime(work.lastMessageAt);
  if (work.messageCount == 0) return '尚无消息';
  return time == null
      ? '${work.messageCount} 条消息'
      : '$time · ${work.messageCount} 条消息';
}

/// Unix 毫秒 → 相对时间（刚刚 / n 分钟前 / n 小时前 / 昨天 / n 天前）。
/// 天数按日历日差计算（不是 24h 窗口），避免跨午夜的「0 天前」。
String? _relativeTime(int? millis) {
  if (millis == null) return null;
  final then = DateTime.fromMillisecondsSinceEpoch(millis);
  final now = DateTime.now();
  final diff = now.difference(then);
  if (diff.inMinutes < 1) return '刚刚';
  if (diff.inHours < 1) return '${diff.inMinutes} 分钟前';
  final days = DateTime(
    now.year,
    now.month,
    now.day,
  ).difference(DateTime(then.year, then.month, then.day)).inDays;
  if (days == 0) return '${diff.inHours} 小时前';
  if (days == 1) return '昨天';
  if (days < 30) return '$days 天前';
  return '${then.month}/${then.day}';
}

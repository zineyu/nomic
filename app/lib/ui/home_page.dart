/// 主界面：左侧 work 列表（侧栏）+ 右侧聊天区。
///
/// 无默认 project/session（ADR-0030 服务端模型）：未选会话时展示启动页
/// （project 选择 + 新 work）。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../platform/file_picker.dart';
import '../protocol/models.dart';
import '../theme.dart';
import 'animations.dart';
import 'chat_page.dart';
import 'mini_icon_button.dart';
import 'settings_page.dart';
import 'sidebar.dart';

class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.controller});

  final AppController controller;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  /// 侧栏搜索框焦点（⌘K 聚焦）。
  final _searchFocusNode = FocusNode();

  @override
  void dispose() {
    _searchFocusNode.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final controller = widget.controller;
    return ListenableBuilder(
      listenable: controller,
      builder: (context, _) {
        // 应用级快捷键：⌘N 新 work；⌘K 聚焦搜索；⌘, 设置开关
        return CallbackShortcuts(
          bindings: {
            const SingleActivator(LogicalKeyboardKey.keyN, meta: true):
                controller.closeSession,
            const SingleActivator(LogicalKeyboardKey.keyK, meta: true): () {
              _searchFocusNode.requestFocus();
            },
            const SingleActivator(LogicalKeyboardKey.comma, meta: true): () {
              if (controller.showingSettings) {
                controller.closeSettings();
              } else {
                controller.openSettings();
              }
            },
          },
          child: Scaffold(
            body: Column(
              children: [
                // 连接横幅：高度 + 透明度收展（不硬跳）
                AnimatedReveal(
                  visible: !controller.connected,
                  child: _ConnectionBanner(controller: controller),
                ),
                Expanded(
                  child: Row(
                    children: [
                      Sidebar(
                        controller: controller,
                        searchFocusNode: _searchFocusNode,
                      ),
                      Container(width: 1, color: tokens.sidebarBorder),
                      Expanded(
                        // 设置 / 聊天 / 启动页切换：交叉淡入 + 轻微位移；
                        // key 只区分页面类型，会话切换不重建 ChatPage 状态
                        child: AnimatedSwitcher(
                          duration: AppMotion.normal,
                          switchInCurve: AppMotion.curve,
                          switchOutCurve: AppMotion.curve,
                          transitionBuilder: (child, animation) =>
                              FadeTransition(
                                opacity: animation,
                                child: SlideTransition(
                                  position: Tween<Offset>(
                                    begin: const Offset(0, 0.015),
                                    end: Offset.zero,
                                  ).animate(animation),
                                  child: child,
                                ),
                              ),
                          child: controller.showingSettings
                              ? SettingsPage(
                                  key: const ValueKey('settings'),
                                  controller: controller,
                                )
                              : controller.hasSession
                              ? ChatPage(
                                  key: const ValueKey('chat'),
                                  controller: controller,
                                )
                              : _StartPage(
                                  key: const ValueKey('start'),
                                  controller: controller,
                                ),
                        ),
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

/// 连接状态横幅：未连接时置顶展示（首次连接中 / 断线重连中）。
class _ConnectionBanner extends StatelessWidget {
  const _ConnectionBanner({required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Material(
      color: tokens.tip,
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Spacing.md,
          vertical: Spacing.sm,
        ),
        child: Row(
          children: [
            SizedBox(
              width: 12,
              height: 12,
              child: CircularProgressIndicator(
                strokeWidth: 2,
                color: tokens.secondary,
              ),
            ),
            const SizedBox(width: Spacing.sm),
            Text(
              controller.hasConnectedOnce ? '连接中断，重连中…' : '连接中…',
              style: AppText.xs(tokens.secondary),
            ),
          ],
        ),
      ),
    );
  }
}

/// 启动页：选择 project 后开始新 work（首条消息在聊天页发送）；
/// 「添加项目目录…」经系统文件选择器登记新 project。
class _StartPage extends StatelessWidget {
  const _StartPage({super.key, required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final projects = controller.projects;
    // 垂直居中 + 内容超出时可滚动（LayoutBuilder 提供 minHeight 约束）
    return LayoutBuilder(
      builder: (context, constraints) {
        return SingleChildScrollView(
          child: ConstrainedBox(
            constraints: BoxConstraints(minHeight: constraints.maxHeight),
            child: Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: maxPageWidth),
                child: Padding(
                  padding: const EdgeInsets.all(Spacing.xl),
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      Text('开始工作', style: AppText.xl(tokens.foreground)),
                      const SizedBox(height: Spacing.sm),
                      Text(
                        '选择一个项目开始，或添加新的项目目录。',
                        style: AppText.s(tokens.secondary),
                      ),
                      const SizedBox(height: Spacing.xl),
                      if (projects.isNotEmpty) ...[
                        Text('项目', style: AppText.xxs(tokens.secondary)),
                        const SizedBox(height: Spacing.sm),
                        for (final project in projects)
                          _ProjectTile(
                            project: project,
                            controller: controller,
                          ),
                        const SizedBox(height: Spacing.sm),
                        _AddProjectTile(onTap: _addProject),
                      ] else ...[
                        _AddProjectTile(onTap: _addProject),
                        const SizedBox(height: Spacing.sm),
                        Text(
                          '还没有项目。添加的项目目录会显示在这里。',
                          style: AppText.xxs(tokens.tertiary),
                        ),
                      ],
                    ],
                  ),
                ),
              ),
            ),
          ),
        );
      },
    );
  }

  /// 系统文件选择器选目录 → 登记 project → 开始新 work。
  Future<void> _addProject() async {
    final path = await FilePicker.pickDirectory();
    if (path == null) return;
    final created = await controller.createProject(path);
    if (created != null) await controller.createWork(created);
  }
}

/// 「添加项目目录…」动作行：细线描边（控件，而非列表条目），hover 出底色。
class _AddProjectTile extends StatelessWidget {
  const _AddProjectTile({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    // 细线描边动作行：与扁平的 project 列表项区分（控件，而非条目）
    return Material(
      color: Colors.transparent,
      borderRadius: BorderRadius.circular(Radii.lg),
      child: InkWell(
        borderRadius: BorderRadius.circular(Radii.lg),
        hoverColor: tokens.primaryDimmed,
        onTap: onTap,
        child: Container(
          decoration: BoxDecoration(
            // 描边控件走 borderStrong（l3）：需要可见分界，l1 发丝线只用于分隔
            border: Border.all(color: tokens.borderStrong),
            borderRadius: BorderRadius.circular(Radii.lg),
          ),
          padding: const EdgeInsets.all(Spacing.md),
          child: Row(
            children: [
              Icon(LucideIcons.plus, size: 16, color: tokens.secondary),
              const SizedBox(width: Spacing.sm),
              Text('添加项目目录…', style: AppText.s(tokens.secondary)),
            ],
          ),
        ),
      ),
    );
  }
}

/// project 行：点击开始新 work；hover 浮现删除入口（icon 按钮）。
class _ProjectTile extends StatefulWidget {
  const _ProjectTile({required this.project, required this.controller});

  final ProjectSummary project;
  final AppController controller;

  @override
  State<_ProjectTile> createState() => _ProjectTileState();
}

class _ProjectTileState extends State<_ProjectTile> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final project = widget.project;
    // 扁平行：hover 才出现底色（whitespace over separators）
    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: Material(
        color: Colors.transparent,
        borderRadius: BorderRadius.circular(Radii.lg),
        child: InkWell(
          borderRadius: BorderRadius.circular(Radii.lg),
          hoverColor: tokens.primaryDimmed,
          onTap: () => unawaited(widget.controller.createWork(project.path)),
          child: Padding(
            padding: const EdgeInsets.all(Spacing.md),
            child: Row(
              children: [
                Icon(LucideIcons.folder, size: 16, color: tokens.secondary),
                const SizedBox(width: Spacing.sm),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        _basename(project.path),
                        overflow: TextOverflow.ellipsis,
                        style: AppText.s(
                          tokens.foreground,
                        ).copyWith(fontWeight: FontWeight.w500),
                      ),
                      Text(
                        project.path,
                        overflow: TextOverflow.ellipsis,
                        style: AppText.xxs(tokens.secondary),
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: Spacing.sm),
                // 右侧：hover 时让位给删除入口（与侧栏 work 行同一模式）；
                // 槽位切换交叉淡入淡出
                AnimatedSwitcher(
                  duration: AppMotion.fast,
                  switchInCurve: AppMotion.curve,
                  switchOutCurve: AppMotion.curve,
                  child: _hovered
                      ? MiniIconButton(
                          key: const ValueKey('delete'),
                          icon: LucideIcons.trash2,
                          tooltip: '删除项目',
                          onTap: () => showDeleteProjectDialog(
                            context,
                            widget.controller,
                            project,
                          ),
                        )
                      : Text(
                          '${project.sessionCount} 个会话',
                          key: const ValueKey('count'),
                          style: AppText.xxs(tokens.secondary),
                        ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// project 路径末段（标题用；完整路径作副标题）。
String _basename(String path) {
  final segments = path.split(RegExp(r'[/\\]')).where((s) => s.isNotEmpty);
  return segments.isEmpty ? path : segments.last;
}

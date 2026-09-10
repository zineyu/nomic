/// 主界面：左侧 work 列表（侧栏）+ 右侧聊天区。
///
/// 无默认 project/session（ADR-0030 服务端模型）：未选会话时展示启动页
/// （project 选择 + 新 work）。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../theme.dart';
import 'chat_page.dart';
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
                if (!controller.connected)
                  _ConnectionBanner(controller: controller),
                Expanded(
                  child: Row(
                    children: [
                      Sidebar(
                        controller: controller,
                        searchFocusNode: _searchFocusNode,
                      ),
                      Container(width: 1, color: tokens.sidebarBorder),
                      Expanded(
                        child: controller.showingSettings
                            ? SettingsPage(controller: controller)
                            : controller.hasSession
                            ? ChatPage(controller: controller)
                            : _StartPage(controller: controller),
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
      color: tokens.muted,
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
                color: tokens.mutedForeground,
              ),
            ),
            const SizedBox(width: Spacing.sm),
            Text(
              controller.hasConnectedOnce ? '连接中断，重连中…' : '连接中…',
              style: AppText.ui(tokens.mutedForeground),
            ),
          ],
        ),
      ),
    );
  }
}

/// 启动页：选择 project 后开始新 work（首条消息在聊天页发送）。
class _StartPage extends StatefulWidget {
  const _StartPage({required this.controller});

  final AppController controller;

  @override
  State<_StartPage> createState() => _StartPageState();
}

class _StartPageState extends State<_StartPage> {
  final _pathController = TextEditingController();

  @override
  void dispose() {
    _pathController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final projects = widget.controller.projects;
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: maxPageWidth),
        child: Padding(
          padding: const EdgeInsets.all(Spacing.xl),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text('选择一个 project 开始', style: AppText.h2(tokens.foreground)),
              const SizedBox(height: Spacing.lg),
              if (projects.isNotEmpty) ...[
                for (final project in projects)
                  _ProjectTile(
                    title: _basename(project.path),
                    subtitle: project.path,
                    trailing: '${project.sessionCount} 个会话',
                    onTap: () => widget.controller.createWork(project.path),
                  ),
                const SizedBox(height: Spacing.lg),
              ],
              TextField(
                controller: _pathController,
                decoration: InputDecoration(
                  hintText: '输入目录路径，登记为新 project…',
                  suffixIcon: IconButton(
                    icon: const Icon(LucideIcons.arrowRight, size: 16),
                    tooltip: '登记并开始',
                    onPressed: () => _submit(_pathController.text),
                  ),
                ),
                onSubmitted: _submit,
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _submit(String path) async {
    final trimmed = path.trim();
    if (trimmed.isEmpty) return;
    final created = await widget.controller.createProject(trimmed);
    if (created != null) {
      _pathController.clear();
      await widget.controller.createWork(created);
    }
  }
}

class _ProjectTile extends StatelessWidget {
  const _ProjectTile({
    required this.title,
    required this.subtitle,
    required this.trailing,
    required this.onTap,
  });

  final String title;
  final String subtitle;
  final String trailing;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    // 扁平行：hover 才出现底色（whitespace over separators）
    return Material(
      color: Colors.transparent,
      borderRadius: BorderRadius.circular(Radii.lg),
      child: InkWell(
        borderRadius: BorderRadius.circular(Radii.lg),
        hoverColor: tokens.secondary,
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(Spacing.md),
          child: Row(
            children: [
              Icon(LucideIcons.folder, size: 16, color: tokens.mutedForeground),
              const SizedBox(width: Spacing.sm),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      title,
                      overflow: TextOverflow.ellipsis,
                      style: AppText.bodySm(
                        tokens.foreground,
                      ).copyWith(fontWeight: FontWeight.w500),
                    ),
                    Text(
                      subtitle,
                      overflow: TextOverflow.ellipsis,
                      style: AppText.caption(tokens.mutedForeground),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: Spacing.sm),
              Text(trailing, style: AppText.caption(tokens.mutedForeground)),
            ],
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

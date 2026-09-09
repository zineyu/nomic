/// 主界面：左侧 work 列表（侧栏）+ 右侧聊天区。
///
/// 无默认 project/session（ADR-0030 服务端模型）：未选会话时展示启动页
/// （project 选择 + 新 work）。
library;

import 'package:flutter/material.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../theme.dart';
import 'chat_page.dart';
import 'sidebar.dart';

class HomePage extends StatelessWidget {
  const HomePage({super.key, required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return ListenableBuilder(
      listenable: controller,
      builder: (context, _) {
        return Scaffold(
          body: Row(
            children: [
              Sidebar(controller: controller),
              Container(width: 1, color: tokens.sidebarBorder),
              Expanded(
                child: controller.hasSession
                    ? ChatPage(controller: controller)
                    : _StartPage(controller: controller),
              ),
            ],
          ),
        );
      },
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
              Text(
                '选择一个 project 开始',
                style: TextStyle(
                  fontSize: 20,
                  fontWeight: FontWeight.w600,
                  color: tokens.foreground,
                ),
              ),
              const SizedBox(height: Spacing.lg),
              if (projects.isNotEmpty) ...[
                for (final project in projects)
                  _ProjectTile(
                    title: project.path,
                    subtitle: '${project.sessionCount} 个会话',
                    onTap: () => widget.controller.createWork(project.path),
                  ),
                const SizedBox(height: Spacing.lg),
              ],
              Row(
                children: [
                  Expanded(
                    child: TextField(
                      controller: _pathController,
                      decoration: const InputDecoration(
                        hintText: '输入目录路径，登记为新 project…',
                      ),
                      onSubmitted: _submit,
                    ),
                  ),
                  const SizedBox(width: Spacing.sm),
                  IconButton(
                    icon: const Icon(LucideIcons.arrowRight, size: 16),
                    tooltip: '登记并开始',
                    onPressed: () => _submit(_pathController.text),
                  ),
                ],
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
    required this.onTap,
  });

  final String title;
  final String subtitle;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: Spacing.sm),
      child: Material(
        color: tokens.card,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(Radii.lg),
          side: BorderSide(color: tokens.border),
        ),
        child: InkWell(
          borderRadius: BorderRadius.circular(Radii.lg),
          onTap: onTap,
          child: Padding(
            padding: const EdgeInsets.all(Spacing.md),
            child: Row(
              children: [
                Icon(
                  LucideIcons.folder,
                  size: 16,
                  color: tokens.mutedForeground,
                ),
                const SizedBox(width: Spacing.sm),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(title, overflow: TextOverflow.ellipsis),
                      Text(
                        subtitle,
                        style: TextStyle(
                          fontSize: 12,
                          color: tokens.mutedForeground,
                        ),
                      ),
                    ],
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

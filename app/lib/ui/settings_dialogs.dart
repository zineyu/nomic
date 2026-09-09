/// 设置编辑对话框：provider 定义与模型覆盖的新建/编辑。
///
/// patch 三态与服务端 `ProviderPatch` / `ModelSpecPatch` 对应：字段缺失 =
/// 不更新，null = 清除，值 = 设置。编辑态清空文本框即清除该字段；新建态
/// 留空即不携带该字段。
library;

import 'package:flutter/material.dart';

import '../app_controller.dart';
import '../protocol/models.dart';
import '../theme.dart';

/// 对话框底部操作行（错误消息内联展示 + 取消/保存）。
class _DialogActions extends StatelessWidget {
  const _DialogActions({required this.saving, required this.onSave});

  final bool saving;
  final VoidCallback onSave;

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisAlignment: MainAxisAlignment.end,
      children: [
        TextButton(
          onPressed: saving ? null : () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        const SizedBox(width: Spacing.sm),
        FilledButton(
          onPressed: saving ? null : onSave,
          child: Text(saving ? '保存中…' : '保存'),
        ),
      ],
    );
  }
}

/// 内联错误消息（保存被服务端拒绝时展示，对话框保持打开）。
Widget _errorText(BuildContext context, String? error) {
  if (error == null) return const SizedBox.shrink();
  final tokens = tokensOf(context);
  return Padding(
    padding: const EdgeInsets.only(top: Spacing.sm),
    child: Text(
      error,
      style: TextStyle(fontSize: 12, color: tokens.destructive),
    ),
  );
}

// ── provider ────────────────────────────────────────────────────────────

class ProviderDialog extends StatefulWidget {
  const ProviderDialog({super.key, required this.controller, this.existing});

  final AppController controller;

  /// 非 null = 编辑（name 不可改）；null = 新建。
  final ProviderView? existing;

  static Future<void> show(
    BuildContext context,
    AppController controller, [
    ProviderView? existing,
  ]) {
    return showDialog<void>(
      context: context,
      builder: (_) =>
          ProviderDialog(controller: controller, existing: existing),
    );
  }

  @override
  State<ProviderDialog> createState() => _ProviderDialogState();
}

class _ProviderDialogState extends State<ProviderDialog> {
  late final TextEditingController _name;
  late final TextEditingController _baseUrl;
  final _apiKey = TextEditingController();

  /// API 种类（null = 按名推断；serde snake_case 值）。
  late String? _api = widget.existing?.api;
  bool _clearApiKey = false;
  bool _saving = false;
  String? _error;

  bool get _creating => widget.existing == null;

  @override
  void initState() {
    super.initState();
    _name = TextEditingController(text: widget.existing?.name ?? '');
    _baseUrl = TextEditingController(text: widget.existing?.baseUrl ?? '');
  }

  @override
  void dispose() {
    _name.dispose();
    _baseUrl.dispose();
    _apiKey.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    final existing = widget.existing;
    final name = existing?.name ?? _name.text.trim();
    if (name.isEmpty) {
      setState(() => _error = 'provider 名不能为空');
      return;
    }
    final patch = <String, Object?>{};
    if (existing == null) {
      // 新建：留空即不携带（api 由服务端按名推断）
      if (_api != null) patch['api'] = _api;
      final url = _baseUrl.text.trim();
      if (url.isNotEmpty) patch['base_url'] = url;
      final key = _apiKey.text.trim();
      if (key.isNotEmpty) patch['api_key'] = key;
    } else {
      // 编辑：偏离现值才入 patch；清空文本框 = 清除该字段
      if (_api != existing.api) patch['api'] = _api;
      final url = _baseUrl.text.trim();
      if (url != (existing.baseUrl ?? '')) {
        patch['base_url'] = url.isEmpty ? null : url;
      }
      final key = _apiKey.text.trim();
      if (key.isNotEmpty) {
        patch['api_key'] = key;
      } else if (_clearApiKey) {
        patch['api_key'] = null;
      }
    }
    setState(() {
      _saving = true;
      _error = null;
    });
    final error = await widget.controller.upsertProvider(name, patch);
    if (!mounted) return;
    if (error == null) {
      Navigator.of(context).pop();
      return;
    }
    setState(() {
      _saving = false;
      _error = error;
    });
  }

  @override
  Widget build(BuildContext context) {
    final existing = widget.existing;
    return AlertDialog(
      title: Text(
        existing == null ? '添加 provider' : '编辑 ${existing.name}',
        style: const TextStyle(fontSize: 16),
      ),
      content: SizedBox(
        width: 420,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            TextField(
              controller: _name,
              enabled: _creating,
              decoration: const InputDecoration(
                labelText: '名称',
                hintText: '如 anthropic / openai / deepseek',
              ),
            ),
            const SizedBox(height: Spacing.md),
            DropdownButtonFormField<String?>(
              initialValue: _api,
              decoration: const InputDecoration(labelText: 'API 种类'),
              items: const [
                DropdownMenuItem(value: null, child: Text('按名推断')),
                DropdownMenuItem(
                  value: 'anthropic_messages',
                  child: Text('Anthropic Messages'),
                ),
                DropdownMenuItem(
                  value: 'open_ai_completions',
                  child: Text('OpenAI Completions'),
                ),
                DropdownMenuItem(
                  value: 'kimi_completions',
                  child: Text('Kimi Completions'),
                ),
              ],
              onChanged: (value) => setState(() => _api = value),
            ),
            const SizedBox(height: Spacing.md),
            TextField(
              controller: _baseUrl,
              decoration: InputDecoration(
                labelText: 'Base URL',
                hintText: _creating ? '留空用默认端点' : '清空即清除该字段',
              ),
            ),
            const SizedBox(height: Spacing.md),
            TextField(
              controller: _apiKey,
              obscureText: true,
              decoration: InputDecoration(
                labelText: 'API key',
                hintText: existing != null && existing.hasApiKey
                    ? '已保存（留空保持不变）'
                    : '建议优先用环境变量，避免明文落库',
              ),
              onChanged: (_) {
                if (_clearApiKey) setState(() => _clearApiKey = false);
              },
            ),
            if (existing != null && existing.hasApiKey)
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: () {
                    setState(() {
                      _apiKey.clear();
                      _clearApiKey = true;
                    });
                  },
                  child: Text(_clearApiKey ? '保存后将清除已保存的 key' : '清除已保存的 key'),
                ),
              ),
            _errorText(context, _error),
          ],
        ),
      ),
      actions: [_DialogActions(saving: _saving, onSave: _save)],
    );
  }
}

// ── 模型覆盖 ────────────────────────────────────────────────────────────

class ModelSpecDialog extends StatefulWidget {
  const ModelSpecDialog({
    super.key,
    required this.controller,
    required this.providers,
    this.existing,
  });

  final AppController controller;

  /// 可选 provider（模型覆盖必须挂在已定义的 provider 下）。
  final List<ProviderView> providers;

  /// 非 null = 编辑（provider / model id 不可改）；null = 新建。
  final ModelSpecRow? existing;

  static Future<void> show(
    BuildContext context,
    AppController controller,
    List<ProviderView> providers, [
    ModelSpecRow? existing,
  ]) {
    return showDialog<void>(
      context: context,
      builder: (_) => ModelSpecDialog(
        controller: controller,
        providers: providers,
        existing: existing,
      ),
    );
  }

  @override
  State<ModelSpecDialog> createState() => _ModelSpecDialogState();
}

class _ModelSpecDialogState extends State<ModelSpecDialog> {
  late String _provider =
      widget.existing?.provider ?? widget.providers.first.name;
  final _modelId = TextEditingController();
  late final TextEditingController _name;
  late final TextEditingController _contextWindow;
  late final TextEditingController _maxTokens;
  late final TextEditingController _costInput;
  late final TextEditingController _costOutput;
  late final TextEditingController _costCacheRead;
  late final TextEditingController _costCacheWrite;

  /// 能力开关三态：null = 不覆盖（编辑态切换到此 = 清除）。
  late bool? _reasoning = widget.existing?.reasoning;
  late bool? _vision = widget.existing?.vision;

  bool _saving = false;
  String? _error;

  bool get _creating => widget.existing == null;

  @override
  void initState() {
    super.initState();
    final existing = widget.existing;
    _modelId.text = existing?.modelId ?? '';
    _name = TextEditingController(text: existing?.displayName ?? '');
    _contextWindow = TextEditingController(
      text: existing?.contextWindow?.toString() ?? '',
    );
    _maxTokens = TextEditingController(
      text: existing?.maxTokens?.toString() ?? '',
    );
    _costInput = TextEditingController(
      text: existing?.costInput?.toString() ?? '',
    );
    _costOutput = TextEditingController(
      text: existing?.costOutput?.toString() ?? '',
    );
    _costCacheRead = TextEditingController(
      text: existing?.costCacheRead?.toString() ?? '',
    );
    _costCacheWrite = TextEditingController(
      text: existing?.costCacheWrite?.toString() ?? '',
    );
  }

  @override
  void dispose() {
    _modelId.dispose();
    _name.dispose();
    _contextWindow.dispose();
    _maxTokens.dispose();
    _costInput.dispose();
    _costOutput.dispose();
    _costCacheRead.dispose();
    _costCacheWrite.dispose();
    super.dispose();
  }

  /// 数字字段入 patch：偏离现值才携带；清空 = 清除；非法数字返回错误消息。
  String? _numField(
    Json patch,
    String key,
    TextEditingController field,
    num? initial, {
    bool integer = false,
  }) {
    final text = field.text.trim();
    if (text == (initial?.toString() ?? '')) return null;
    if (text.isEmpty) {
      patch[key] = null;
      return null;
    }
    final value = integer ? int.tryParse(text) : num.tryParse(text);
    if (value == null || (value is int && value < 0)) {
      return '$key 不是合法数字：$text';
    }
    patch[key] = value;
    return null;
  }

  Future<void> _save() async {
    final existing = widget.existing;
    final modelId = existing?.modelId ?? _modelId.text.trim();
    if (modelId.isEmpty || modelId.contains(RegExp(r'\s'))) {
      setState(() => _error = '模型 id 非法：非空且不能含空白字符');
      return;
    }
    final patch = <String, Object?>{};
    final name = _name.text.trim();
    if (name != (existing?.displayName ?? '')) {
      patch['name'] = name.isEmpty ? null : name;
    }
    if (_reasoning != existing?.reasoning) patch['reasoning'] = _reasoning;
    if (_vision != existing?.vision) patch['vision'] = _vision;
    final error =
        _numField(
          patch,
          'context_window',
          _contextWindow,
          existing?.contextWindow,
          integer: true,
        ) ??
        _numField(
          patch,
          'max_tokens',
          _maxTokens,
          existing?.maxTokens,
          integer: true,
        ) ??
        _numField(patch, 'cost_input', _costInput, existing?.costInput) ??
        _numField(patch, 'cost_output', _costOutput, existing?.costOutput) ??
        _numField(
          patch,
          'cost_cache_read',
          _costCacheRead,
          existing?.costCacheRead,
        ) ??
        _numField(
          patch,
          'cost_cache_write',
          _costCacheWrite,
          existing?.costCacheWrite,
        );
    if (error != null) {
      setState(() => _error = error);
      return;
    }
    setState(() {
      _saving = true;
      _error = null;
    });
    final failure = await widget.controller.upsertModelSpec(
      _provider,
      modelId,
      patch,
    );
    if (!mounted) return;
    if (failure == null) {
      Navigator.of(context).pop();
      return;
    }
    setState(() {
      _saving = false;
      _error = failure;
    });
  }

  @override
  Widget build(BuildContext context) {
    final existing = widget.existing;
    return AlertDialog(
      title: Text(
        existing == null ? '添加模型覆盖' : '编辑 ${existing.spec}',
        style: const TextStyle(fontSize: 16),
      ),
      content: SizedBox(
        width: 420,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              DropdownButtonFormField<String>(
                initialValue: _provider,
                decoration: const InputDecoration(labelText: 'Provider'),
                items: [
                  for (final provider in widget.providers)
                    DropdownMenuItem(
                      value: provider.name,
                      child: Text(provider.name),
                    ),
                ],
                onChanged: _creating
                    ? (value) {
                        if (value != null) setState(() => _provider = value);
                      }
                    : null,
              ),
              const SizedBox(height: Spacing.md),
              TextField(
                controller: _modelId,
                enabled: _creating,
                decoration: const InputDecoration(
                  labelText: '模型 id',
                  hintText: '如 gpt-5.2 / claude-sonnet-4-5',
                ),
              ),
              const SizedBox(height: Spacing.md),
              TextField(
                controller: _name,
                decoration: const InputDecoration(labelText: '展示名'),
              ),
              const SizedBox(height: Spacing.md),
              Row(
                children: [
                  Expanded(
                    child: _BoolField(
                      label: '推理/思考',
                      value: _reasoning,
                      onChanged: (value) => setState(() => _reasoning = value),
                    ),
                  ),
                  const SizedBox(width: Spacing.md),
                  Expanded(
                    child: _BoolField(
                      label: '图像输入',
                      value: _vision,
                      onChanged: (value) => setState(() => _vision = value),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: Spacing.md),
              Row(
                children: [
                  Expanded(
                    child: TextField(
                      controller: _contextWindow,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(
                        labelText: '上下文窗口（tokens）',
                      ),
                    ),
                  ),
                  const SizedBox(width: Spacing.md),
                  Expanded(
                    child: TextField(
                      controller: _maxTokens,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(
                        labelText: '最大输出（tokens）',
                      ),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: Spacing.md),
              Text(
                '每百万 token 费率（清空即清除；留空即不覆盖）',
                style: TextStyle(
                  fontSize: 12,
                  color: tokensOf(context).mutedForeground,
                ),
              ),
              const SizedBox(height: Spacing.sm),
              Row(
                children: [
                  Expanded(
                    child: TextField(
                      controller: _costInput,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: '输入'),
                    ),
                  ),
                  const SizedBox(width: Spacing.sm),
                  Expanded(
                    child: TextField(
                      controller: _costOutput,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: '输出'),
                    ),
                  ),
                  const SizedBox(width: Spacing.sm),
                  Expanded(
                    child: TextField(
                      controller: _costCacheRead,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: '缓存读'),
                    ),
                  ),
                  const SizedBox(width: Spacing.sm),
                  Expanded(
                    child: TextField(
                      controller: _costCacheWrite,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: '缓存写'),
                    ),
                  ),
                ],
              ),
              _errorText(context, _error),
            ],
          ),
        ),
      ),
      actions: [_DialogActions(saving: _saving, onSave: _save)],
    );
  }
}

/// 能力开关三态下拉（null = 不覆盖）。
class _BoolField extends StatelessWidget {
  const _BoolField({
    required this.label,
    required this.value,
    required this.onChanged,
  });

  final String label;
  final bool? value;
  final ValueChanged<bool?> onChanged;

  @override
  Widget build(BuildContext context) {
    return DropdownButtonFormField<bool?>(
      initialValue: value,
      decoration: InputDecoration(labelText: label),
      items: const [
        DropdownMenuItem(value: null, child: Text('不覆盖')),
        DropdownMenuItem(value: true, child: Text('支持')),
        DropdownMenuItem(value: false, child: Text('不支持')),
      ],
      onChanged: onChanged,
    );
  }
}

/// 与 Rust 侧 serde JSON 对应的模型（字段名保持 snake_case，与 nomic 的 serde
/// 输出一致）。参见 crates/runtime/nomic-ai/src/types.rs 与
/// crates/app/nomic-cli/src/serve/api/handlers.rs。
///
/// 协议为纯 WebSocket 事件流（ADR-0030 服务端 / ADR-0046 Flutter GUI）：
/// 解析防御式（字段缺失给默认值），协议演进时旧版本客户端不崩。
library;

typedef Json = Map<String, dynamic>;

String asStr(Object? v, [String fallback = '']) => v is String ? v : fallback;
String? asStrOrNull(Object? v) => v is String ? v : null;
int asInt(Object? v, [int fallback = 0]) =>
    v is int ? v : (v is num ? v.toInt() : fallback);
int? asIntOrNull(Object? v) => v is int ? v : (v is num ? v.toInt() : null);
bool asBool(Object? v, [bool fallback = false]) => v is bool ? v : fallback;
num? asNumOrNull(Object? v) => v is num ? v : null;
Json asJson(Object? v) => v is Map<String, dynamic> ? v : const {};
List<Json> asJsonList(Object? v) =>
    v is List ? v.whereType<Map<String, dynamic>>().toList() : const [];

// ── 消息模型（nomic-ai types）─────────────────────────────────────────────

/// token 用量（assistant 消息携带）。
class Usage {
  const Usage({
    required this.input,
    required this.output,
    required this.totalTokens,
  });

  factory Usage.fromJson(Json json) => Usage(
    input: asInt(json['input']),
    output: asInt(json['output']),
    totalTokens: asInt(json['total_tokens']),
  );

  final int input;
  final int output;
  final int totalTokens;
}

/// assistant 消息内容块（内部标签 `type`：text / thinking / tool_call）。
class ContentBlock {
  const ContentBlock({required this.type, required this.raw});

  factory ContentBlock.fromJson(Json json) =>
      ContentBlock(type: asStr(json['type']), raw: json);

  final String type;
  final Json raw;

  String get text => asStr(raw['text']);
  String get thinking => asStr(raw['thinking']);
  String get toolCallId => asStr(raw['id']);
  String get toolName => asStr(raw['name']);
  Json get arguments => asJson(raw['arguments']);
}

/// 会话消息（user / assistant / tool_result 三种 role）。
class Message {
  const Message({required this.role, required this.raw});

  factory Message.fromJson(Json json) =>
      Message(role: asStr(json['role']), raw: json);

  final String role;
  final Json raw;

  bool get isUser => role == 'user';
  bool get isAssistant => role == 'assistant';
  bool get isToolResult => role == 'tool_result';

  /// 用户消息文本（content 为 string 或 text/image 块数组）。
  String get userText {
    final content = raw['content'];
    if (content is String) return content;
    return asJsonList(
      content,
    ).where((b) => b['type'] == 'text').map((b) => asStr(b['text'])).join();
  }

  /// 用户消息图片附件。
  List<Json> get userImages {
    final content = raw['content'];
    if (content is! List) return const [];
    return asJsonList(content).where((b) => b['type'] == 'image').toList();
  }

  /// assistant 内容块。
  List<ContentBlock> get blocks =>
      asJsonList(raw['content']).map(ContentBlock.fromJson).toList();

  String get assistantText =>
      blocks.where((b) => b.type == 'text').map((b) => b.text).join();
  String get assistantThinking => blocks
      .where((b) => b.type == 'thinking')
      .map((b) => b.thinking)
      .join('\n');

  String get model => asStr(raw['model']);
  String get stopReason => asStr(raw['stop_reason'], 'stop');
  String? get errorMessage => asStrOrNull(raw['error_message']);
  Usage? get usage =>
      raw['usage'] is Map ? Usage.fromJson(asJson(raw['usage'])) : null;
  int get timestamp => asInt(raw['timestamp']);

  /// tool_result 字段。
  String get toolCallId => asStr(raw['tool_call_id']);
  String get toolName => asStr(raw['tool_name']);
  bool get isError => asBool(raw['is_error']);
  String get resultText {
    return asJsonList(
      raw['content'],
    ).where((b) => b['type'] == 'text').map((b) => asStr(b['text'])).join('\n');
  }
}

// ── 模型（nomic-cli model）────────────────────────────────────────────────

/// 会话当前模型（快照携带）。
class ModelInfo {
  const ModelInfo({
    required this.id,
    required this.name,
    required this.provider,
    required this.reasoning,
  });

  factory ModelInfo.fromJson(Json json) => ModelInfo(
    id: asStr(json['id']),
    name: asStr(json['name']),
    provider: asStr(json['provider']),
    reasoning: asBool(json['reasoning']),
  );

  final String id;
  final String name;
  final String provider;
  final bool reasoning;

  static const placeholder = ModelInfo(
    id: '',
    name: '(未选择模型)',
    provider: '',
    reasoning: false,
  );
}

/// 候选模型（`list_models` 响应）。
class ModelChoice {
  const ModelChoice({
    required this.provider,
    required this.id,
    required this.name,
    required this.contextWindow,
    required this.reasoning,
  });

  factory ModelChoice.fromJson(Json json) => ModelChoice(
    provider: asStr(json['provider']),
    id: asStr(json['id']),
    name: asStr(json['name']),
    contextWindow: asInt(json['context_window']),
    reasoning: asBool(json['reasoning']),
  );

  final String provider;
  final String id;
  final String name;
  final int contextWindow;
  final bool reasoning;

  String get spec => '$provider/$id';
}

// ── 会话 / work / project 摘要（nomic-session）────────────────────────────

/// work 摘要（侧栏列表的一等入口，ADR-0044）；点击打开 `mainSessionId`。
class WorkSummary {
  const WorkSummary({
    required this.id,
    required this.mainSessionId,
    required this.title,
    required this.project,
    required this.sessionCount,
    required this.messageCount,
    required this.lastMessageAt,
  });

  factory WorkSummary.fromJson(Json json) => WorkSummary(
    id: asStr(json['id']),
    mainSessionId: asStr(json['main_session_id']),
    title: asStrOrNull(json['title']),
    project: asStr(json['project']),
    sessionCount: asInt(json['session_count']),
    messageCount: asInt(json['message_count']),
    lastMessageAt: asIntOrNull(json['last_message_at']),
  );

  final String id;
  final String mainSessionId;
  final String? title;
  final String project;
  final int sessionCount;
  final int messageCount;

  /// Unix 毫秒；无消息时为 null。
  final int? lastMessageAt;

  /// 展示标题（无消息时回退为「新会话」）。
  String get displayTitle => title ?? '新会话';
}

/// project 摘要（启动页选择与侧栏分组）。
class ProjectSummary {
  const ProjectSummary({
    required this.id,
    required this.path,
    required this.sessionCount,
  });

  factory ProjectSummary.fromJson(Json json) => ProjectSummary(
    id: asStr(json['id']),
    path: asStr(json['path']),
    sessionCount: asInt(json['session_count']),
  );

  final String id;
  final String path;
  final int sessionCount;
}

// ── 运行状态 ──────────────────────────────────────────────────────────────

/// steering 队列条目（快照与 queue_changed 事件携带；images 仅回传附件数）。
class QueueEntry {
  const QueueEntry({
    required this.id,
    required this.text,
    required this.images,
  });

  factory QueueEntry.fromJson(Json json) => QueueEntry(
    id: asStr(json['id']),
    text: asStr(json['text']),
    images: asInt(json['images']),
  );

  final String id;
  final String text;
  final int images;
}

/// `ask_user_question` 提问（kind：single_choice / multiple_choice / fill_in）。
class AskUserQuestion {
  const AskUserQuestion({
    required this.question,
    required this.kind,
    required this.options,
  });

  factory AskUserQuestion.fromJson(Json json) => AskUserQuestion(
    question: asStr(json['question']),
    kind: asStr(json['kind'], 'fill_in'),
    options: json['options'] is List
        ? (json['options']! as List).map((o) => o.toString()).toList()
        : const [],
  );

  final String question;
  final String kind;
  final List<String> options;

  bool get isSingle => kind == 'single_choice';
  bool get isMultiple => kind == 'multiple_choice';
}

/// 在途提问（快照 `pending_question` 字段；Rust 侧为元组 `[id, question]`）。
class PendingQuestion {
  const PendingQuestion({required this.id, required this.question});

  /// 防御式解析：兼容元组数组形式（当前服务端）与对象形式（旧前端类型标注）。
  static PendingQuestion? fromJson(Object? v) {
    if (v is List && v.length == 2) {
      return PendingQuestion(
        id: asStr(v[0]),
        question: AskUserQuestion.fromJson(asJson(v[1])),
      );
    }
    if (v is Map<String, dynamic>) {
      return PendingQuestion(
        id: asStr(v['id']),
        question: AskUserQuestion.fromJson(asJson(v['question'])),
      );
    }
    return null;
  }

  final String id;
  final AskUserQuestion question;
}

/// 会话统计信息（状态栏展示；快照 flatten 携带）。
class SessionStats {
  const SessionStats({
    this.rounds = 0,
    this.totalSteps = 0,
    this.inputTokens = 0,
    this.outputTokens = 0,
  });

  factory SessionStats.fromJson(Json json) => SessionStats(
    rounds: asInt(json['rounds']),
    totalSteps: asInt(json['total_steps']),
    inputTokens: asInt(json['input_tokens']),
    outputTokens: asInt(json['output_tokens']),
  );

  final int rounds;
  final int totalSteps;
  final int inputTokens;
  final int outputTokens;
}

// ── 设置（ADR-0039；settings.rs 三表）────────────────────────────────────

/// 标量设置键（`settings` 表；与 crates/app/nomic-cli/src/settings.rs `keys` 对应）。
abstract final class SettingKeys {
  /// 追加到系统提示词末尾的文本
  static const appendSystem = 'append_system';

  /// 额外的 prompt template 文件或目录
  static const prompts = 'prompts';

  /// 自动压缩开关
  static const compactionEnabled = 'compaction.enabled';

  /// 模型别名表（别名 → `<provider>/<模型id>`）
  static const modelAliases = 'model_aliases';
}

/// provider 定义（`get_settings` 快照携带；api_key 已脱敏为 hasApiKey）。
class ProviderView {
  const ProviderView({
    required this.name,
    this.api,
    this.baseUrl,
    required this.hasApiKey,
    required this.updatedAt,
  });

  factory ProviderView.fromJson(Json json) => ProviderView(
    name: asStr(json['name']),
    api: asStrOrNull(json['api']),
    baseUrl: asStrOrNull(json['base_url']),
    hasApiKey: asBool(json['has_api_key']),
    updatedAt: asInt(json['updated_at']),
  );

  final String name;

  /// API 种类（serde snake_case；null = 按名推断）。
  final String? api;
  final String? baseUrl;
  final bool hasApiKey;
  final int updatedAt;
}

/// 模型覆盖行（`model_specs` 表；spec 字段经 serde flatten 进同一 JSON 对象，
/// null 字段不下发——缺失即「未覆盖」）。
class ModelSpecRow {
  const ModelSpecRow({
    required this.provider,
    required this.modelId,
    required this.raw,
    required this.updatedAt,
  });

  factory ModelSpecRow.fromJson(Json json) => ModelSpecRow(
    provider: asStr(json['provider']),
    modelId: asStr(json['model_id']),
    raw: json,
    updatedAt: asInt(json['updated_at']),
  );

  final String provider;
  final String modelId;

  /// 原始 JSON（flatten 的 spec 字段经 getter 读取）。
  final Json raw;
  final int updatedAt;

  String get spec => '$provider/$modelId';
  String? get displayName => asStrOrNull(raw['name']);
  bool? get reasoning => raw['reasoning'] as bool?;
  bool? get vision => raw['vision'] as bool?;
  int? get contextWindow => asIntOrNull(raw['context_window']);
  int? get maxTokens => asIntOrNull(raw['max_tokens']);
  num? get costInput => asNumOrNull(raw['cost_input']);
  num? get costOutput => asNumOrNull(raw['cost_output']);
  num? get costCacheRead => asNumOrNull(raw['cost_cache_read']);
  num? get costCacheWrite => asNumOrNull(raw['cost_cache_write']);

  /// 覆盖字段摘要（列表副标题用；无覆盖字段时为空串）。
  String get summary => [
    ?displayName,
    if (reasoning != null) reasoning! ? 'reasoning' : '无 reasoning',
    if (vision != null) vision! ? 'vision' : '无 vision',
    if (contextWindow != null) 'ctx $contextWindow',
    if (maxTokens != null) 'max $maxTokens',
    if (costInput != null) 'in \$$costInput/M',
    if (costOutput != null) 'out \$$costOutput/M',
    if (costCacheRead != null) 'cache读 \$$costCacheRead/M',
    if (costCacheWrite != null) 'cache写 \$$costCacheWrite/M',
  ].join(' · ');
}

/// 设置快照（`get_settings` 响应负载）。
class SettingsSnapshot {
  const SettingsSnapshot({
    required this.providers,
    required this.modelSpecs,
    required this.settings,
    required this.scalarKeys,
  });

  factory SettingsSnapshot.fromJson(Json json) => SettingsSnapshot(
    providers: asJsonList(
      json['providers'],
    ).map(ProviderView.fromJson).toList(),
    modelSpecs: asJsonList(
      json['model_specs'],
    ).map(ModelSpecRow.fromJson).toList(),
    settings: asJson(json['settings']),
    scalarKeys: json['scalar_keys'] is List
        ? (json['scalar_keys']! as List).whereType<String>().toList()
        : const [],
  );

  final List<ProviderView> providers;
  final List<ModelSpecRow> modelSpecs;

  /// 标量设置全量（键 → JSON 值；未设置的键缺失）。
  final Json settings;
  final List<String> scalarKeys;

  bool isSet(String key) => settings.containsKey(key);
  Object? operator [](String key) => settings[key];

  /// 自动压缩开关（未设置时回退内置默认：开启）。
  bool get compactionEnabled => settings[SettingKeys.compactionEnabled] is bool
      ? settings[SettingKeys.compactionEnabled]! as bool
      : true;

  /// 追加系统提示词（未设置为空串）。
  String get appendSystem => asStr(settings[SettingKeys.appendSystem]);

  /// 额外 prompt template 路径列表。
  List<String> get promptPaths {
    final raw = settings[SettingKeys.prompts];
    if (raw is! List) return const [];
    return raw.map((e) => e.toString()).toList();
  }

  /// 模型别名表（别名 → `<provider>/<模型id>`）。
  Map<String, String> get modelAliases {
    final raw = settings[SettingKeys.modelAliases];
    if (raw is! Map) return const {};
    return raw.map((k, v) => MapEntry(k.toString(), v.toString()));
  }
}

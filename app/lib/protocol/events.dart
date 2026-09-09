/// 服务端 / 客户端事件协议（纯 WebSocket 事件流；ADR-0030 服务端，ADR-0046 GUI）。
///
/// - 服务端→客户端：`ServerEvent`（内部标签 `type`，snake_case；见
///   `crates/app/nomic-cli/src/serve/mod.rs`）
/// - 客户端→服务端：`ClientEvent`（同结构；见
///   `crates/app/nomic-cli/src/serve/api.rs`）
/// - `agent` 事件负载为 `AgentEvent`（外部标签形式：unit 变体为字符串，
///   带负载变体为 `{"VariantName": {...}}`；见
///   `crates/runtime/nomic-core/src/agent/events.rs`）
library;

import 'models.dart';

/// 提取外部标签枚举的变体名（`"AgentStart"` 或 `{"AgentEnd": {...}}`）。
String eventKind(Object? event) {
  if (event is String) return event;
  if (event is Map<String, dynamic> && event.isNotEmpty) {
    return event.keys.first;
  }
  return '';
}

/// 提取外部标签枚举的负载（unit 变体返回空 map）。
Json eventPayload(Object? event) {
  if (event is Map<String, dynamic> && event.isNotEmpty) {
    return asJson(event.values.first);
  }
  return const {};
}

/// 服务端推送事件（所有生命周期事件携带 `sessionId` 供路由）。
class ServerEvent {
  const ServerEvent({required this.type, required this.raw});

  /// 解析 text frame；无法识别时返回 type 为空串的事件（调用方忽略）。
  factory ServerEvent.fromJson(Json json) =>
      ServerEvent(type: asStr(json['type']), raw: json);

  final String type;
  final Json raw;

  String get sessionId => asStr(raw['session_id']);
  String get requestId => asStr(raw['request_id']);
  String get message => asStr(raw['message']);

  // ── 生命周期事件 ────────────────────────────────────────────────────
  bool get isAgent => type == 'agent';

  /// `agent` 事件的负载（AgentEvent，外部标签形式）。
  Object? get agentEvent => raw['event'];

  bool get isQuestion => type == 'question';
  bool get isQuestionCancelled => type == 'question_cancelled';
  bool get isRunStarted => type == 'run_started';
  bool get isRunFinished => type == 'run_finished';
  bool get isError => type == 'error';
  bool get isRefresh => type == 'refresh';
  bool get isQueueChanged => type == 'queue_changed';
  bool get isGoalChanged => type == 'goal_changed';

  /// `question` 事件的提问 id 与内容。
  String get questionId => asStr(raw['id']);
  AskUserQuestion get question =>
      AskUserQuestion.fromJson(asJson(raw['question']));

  /// `queue_changed` 的全量队列快照。
  List<QueueEntry> get queue =>
      asJsonList(raw['queue']).map(QueueEntry.fromJson).toList();

  /// `goal_changed` 的状态（started / completed / cancelled）与目标原文。
  String get goalStatus => asStr(raw['status']);
  String? get goalObjective => asStrOrNull(raw['objective']);

  // ── 列表变化广播（驱动侧栏刷新）──────────────────────────────────────
  bool get isListChanged =>
      type == 'work_created' ||
      type == 'work_deleted' ||
      type == 'work_renamed' ||
      type == 'project_created' ||
      type == 'project_deleted' ||
      type == 'session_deleted';
}

/// 客户端事件构造（序列化为 JSON text frame）。
class ClientEvent {
  ClientEvent._(this.payload);

  final Json payload;

  /// 查询当前会话快照。
  static Json getState(String sessionId, String requestId) => {
    'type': 'get_state',
    'session_id': sessionId,
    'request_id': requestId,
  };

  static Json listModels(String requestId) => {
    'type': 'list_models',
    'request_id': requestId,
  };

  static Json listWorks(String requestId) => {
    'type': 'list_works',
    'request_id': requestId,
  };

  static Json listProjects(String requestId) => {
    'type': 'list_projects',
    'request_id': requestId,
  };

  static Json listWorkSessions(String requestId, String workId) => {
    'type': 'list_work_sessions',
    'request_id': requestId,
    'work_id': workId,
  };

  /// 新建 work（连带创建主 session；响应 `work_created` 携带二者 id）。
  static Json createWork(String requestId, String project) => {
    'type': 'create_work',
    'request_id': requestId,
    'project': project,
  };

  /// 登记新 project（查或插，幂等）。
  static Json createProject(String requestId, String path) => {
    'type': 'create_project',
    'request_id': requestId,
    'path': path,
  };

  static Json deleteWork(String requestId, String id) => {
    'type': 'delete_work',
    'request_id': requestId,
    'id': id,
  };

  static Json renameWork(String requestId, String id, String title) => {
    'type': 'rename_work',
    'request_id': requestId,
    'id': id,
    'title': title,
  };

  /// 提交 prompt（空闲即跑；运行中入 steering 队列）。
  static Json prompt(String sessionId, String text) => {
    'type': 'prompt',
    'session_id': sessionId,
    'text': text,
  };

  /// 取消当前轮运行。
  static Json cancel(String sessionId) => {
    'type': 'cancel',
    'session_id': sessionId,
  };

  /// 回答提问。
  static Json answerQuestion(
    String sessionId,
    String id,
    List<String> answers, {
    String? custom,
  }) => {
    'type': 'answer_question',
    'session_id': sessionId,
    'id': id,
    'answers': answers,
    'custom': ?custom,
  };

  /// 切换会话模型。
  static Json switchModel(String sessionId, String spec) => {
    'type': 'switch_model',
    'session_id': sessionId,
    'spec': spec,
  };
}

/// 应用控制层：连接管理 + 当前会话状态（快照 + 事件增量合并）。
///
/// 语义移植自原 web 前端 `useChat`（ADR-0046）：单个 WebSocket 连接自动接收
/// 全局事件总线上的全部 session 事件，仅当前查看 session 的事件驱动 UI；
/// 重连后重新拉取快照补齐。
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import 'net/ws_client.dart';
import 'protocol/events.dart';
import 'protocol/models.dart';
import 'state/chat_items.dart';

class AppController extends ChangeNotifier {
  AppController({required String url}) : _client = WsClient(url: url) {
    _client.onConnectionChanged = notifyListeners;
    _subscription = _client.events.listen(_onEvent);
  }

  final WsClient _client;
  late final StreamSubscription<ServerEvent> _subscription;

  // ── 连接与全局列表 ─────────────────────────────────────────────────────

  /// 是否已连接（未连接时 UI 展示连接中状态）。
  bool get connected => _client.connected;

  /// 是否成功连接过（区分「首次连接中」与「断线重连中」两种横幅文案）。
  bool get hasConnectedOnce => _client.hasConnectedOnce;

  List<WorkSummary> works = [];
  List<ProjectSummary> projects = [];

  /// 全局错误（连接失败、请求错误等；banner 展示后可清除）。
  String? error;

  // ── 当前会话 ───────────────────────────────────────────────────────────

  String? sessionId;
  List<ChatItem> items = [];
  bool running = false;
  List<QueueEntry> queue = [];
  ModelInfo model = ModelInfo.placeholder;
  List<ModelChoice> modelCandidates = [];
  String? reasoning;
  int contextTokens = 0;
  String? project;

  /// 父 session id（子 agent session 血缘；非 null 时只读回溯，不可发送）
  String? parentSessionId;
  PendingQuestion? question;
  String? goal;
  SessionStats stats = const SessionStats();

  bool get readOnly => parentSessionId != null;
  bool get hasSession => sessionId != null;

  // ── 初始化 ─────────────────────────────────────────────────────────────

  /// 启动：连接事件流并拉取侧栏列表（不预建 session，启动页选 project 后
  /// 显式创建——ADR-0030 服务端模型）。
  Future<void> start() async {
    try {
      await _client.connect();
    } catch (_) {
      // 连接失败由重连机制接管
    }
    await refreshLists();
  }

  /// 刷新 work / project 列表（列表变化广播后调用）。
  Future<void> refreshLists() async {
    try {
      final worksResult = await _client.request(ClientEvent.listWorks);
      works = asJsonList(
        worksResult['works'],
      ).map(WorkSummary.fromJson).toList();
      final projectsResult = await _client.request(ClientEvent.listProjects);
      projects = asJsonList(
        projectsResult['projects'],
      ).map(ProjectSummary.fromJson).toList();
      error = null;
    } on ServerException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  // ── 会话操作 ───────────────────────────────────────────────────────────

  /// 跳出当前会话（返回启动页选择 project）。
  void closeSession() {
    sessionId = null;
    items = [];
    question = null;
    goal = null;
    showingSettings = false;
    notifyListeners();
  }

  /// 打开已有 session（work 的主 session 或历史 session）：拉取快照。
  Future<void> openSession(String id) async {
    sessionId = id;
    items = [];
    running = false;
    queue = [];
    question = null;
    goal = null;
    showingSettings = false;
    notifyListeners();
    await _refreshSnapshot();
  }

  /// 新建 work（启动页选定 project 后）：服务端连带创建主 session，
  /// 响应 `work_created.session_id` 即打开目标。
  Future<void> createWork(String projectPath) async {
    try {
      final result = await _client.request(
        (id) => ClientEvent.createWork(id, projectPath),
      );
      await refreshLists();
      await openSession(asStr(result['session_id']));
    } on ServerException catch (e) {
      error = e.message;
      notifyListeners();
    }
  }

  /// 登记新 project（查或插，幂等）。
  Future<String?> createProject(String path) async {
    try {
      final result = await _client.request(
        (id) => ClientEvent.createProject(id, path),
      );
      await refreshLists();
      return asStr(result['path']);
    } on ServerException catch (e) {
      error = e.message;
      notifyListeners();
      return null;
    }
  }

  Future<void> deleteWork(String workId) async {
    try {
      await _client.request((id) => ClientEvent.deleteWork(id, workId));
      await refreshLists();
    } on ServerException catch (e) {
      error = e.message;
      notifyListeners();
    }
  }

  // ── 设置（ADR-0039）────────────────────────────────────────────────────

  /// 设置页是否打开（右侧面板在 设置页 / 聊天页 / 启动页 间切换）。
  bool showingSettings = false;

  /// 设置快照（打开设置页时拉取；settings_changed 广播驱动刷新）。
  SettingsSnapshot? settings;

  void openSettings() {
    showingSettings = true;
    notifyListeners();
    unawaited(loadSettings());
  }

  void closeSettings() {
    showingSettings = false;
    notifyListeners();
  }

  Future<void> loadSettings() async {
    try {
      final result = await _client.request(ClientEvent.getSettings);
      settings = SettingsSnapshot.fromJson(asJson(result['snapshot']));
      error = null;
    } on ServerException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// 设置类写操作：成功返回 null，失败返回服务端错误消息（表单内联展示）。
  Future<String?> _mutateSettings(Json Function(String requestId) build) async {
    try {
      await _client.request(build);
      return null;
    } on ServerException catch (e) {
      return e.message;
    }
  }

  Future<String?> setScalar(String key, Object? value) =>
      _mutateSettings((id) => ClientEvent.setSetting(id, key, value));
  Future<String?> unsetScalar(String key) =>
      _mutateSettings((id) => ClientEvent.unsetSetting(id, key));
  Future<String?> upsertProvider(String name, Json patch) =>
      _mutateSettings((id) => ClientEvent.upsertProvider(id, name, patch));
  Future<String?> deleteProvider(String name) =>
      _mutateSettings((id) => ClientEvent.deleteProvider(id, name));
  Future<String?> upsertModelSpec(
    String provider,
    String modelId,
    Json patch,
  ) => _mutateSettings(
    (id) => ClientEvent.upsertModelSpec(id, provider, modelId, patch),
  );
  Future<String?> deleteModelSpec(String provider, String modelId) =>
      _mutateSettings(
        (id) => ClientEvent.deleteModelSpec(id, provider, modelId),
      );

  /// 发送 prompt（空闲即跑；运行中入 steering 队列，与服务端同一语义）。
  void send(String text) {
    final id = sessionId;
    if (id == null || text.trim().isEmpty || readOnly) return;
    _client.send(ClientEvent.prompt(id, text));
  }

  /// 取消当前轮运行（排队 job 保留）。
  void cancel() {
    final id = sessionId;
    if (id == null) return;
    _client.send(ClientEvent.cancel(id));
  }

  /// 删除 steering 队列条目（fire-and-forget；`queue_changed` 广播回填）。
  void removeQueueEntry(String id) {
    final sid = sessionId;
    if (sid == null) return;
    _client.send(ClientEvent.removeQueueEntry(sid, id));
  }

  /// 移动 steering 队列条目（up = 向队首方向移一位）。
  void moveQueueEntry(String id, {required bool up}) {
    final sid = sessionId;
    if (sid == null) return;
    _client.send(ClientEvent.moveQueueEntry(sid, id, up ? 'up' : 'down'));
  }

  /// 回答提问。
  void answerQuestion(List<String> answers, {String? custom}) {
    final id = sessionId;
    final q = question;
    if (id == null || q == null) return;
    _client.send(ClientEvent.answerQuestion(id, q.id, answers, custom: custom));
    question = null;
    notifyListeners();
  }

  /// 加载候选模型列表（打开模型选择器时调用）。
  Future<void> loadModels() async {
    try {
      final result = await _client.request(ClientEvent.listModels);
      modelCandidates = asJsonList(
        result['candidates'],
      ).map(ModelChoice.fromJson).toList();
      error = null;
    } on ServerException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// 切换会话模型。
  void switchModel(String spec) {
    final id = sessionId;
    if (id == null) return;
    _client.send(ClientEvent.switchModel(id, spec));
  }

  void clearError() {
    error = null;
    notifyListeners();
  }

  // ── 事件处理 ───────────────────────────────────────────────────────────

  void _onEvent(ServerEvent event) {
    // 列表变化广播（所有客户端）：刷新侧栏
    if (event.isListChanged) {
      unawaited(refreshLists());
    }
    // 重连后补齐：重新拉取当前 session 快照
    if (event.isRefresh) {
      unawaited(_refreshSnapshot());
      if (settings != null) unawaited(loadSettings());
      return;
    }
    // 设置变化广播（无 session 维度）：设置页打开时重新拉取快照
    if (event.isSettingsChanged) {
      if (settings != null) unawaited(loadSettings());
      return;
    }
    // 仅当前查看 session 的事件驱动 UI（后台 session 事件忽略；
    // 与原 web 前端 useChat 同一口径）
    if (event.sessionId.isNotEmpty && event.sessionId != sessionId) {
      return;
    }

    var changed = true;
    if (event.isAgent) {
      final tokens = applyAgentEvent(items, event.agentEvent);
      if (tokens != null) contextTokens = tokens;
    } else if (event.isRunStarted) {
      running = true;
    } else if (event.isRunFinished) {
      running = false;
    } else if (event.isQuestion) {
      question = PendingQuestion(
        id: event.questionId,
        question: event.question,
      );
    } else if (event.isQuestionCancelled) {
      if (question?.id == event.questionId) question = null;
    } else if (event.isQueueChanged) {
      queue = event.queue;
    } else if (event.isGoalChanged) {
      goal = event.goalStatus == 'started' ? event.goalObjective : null;
    } else if (event.isError) {
      error = event.message;
    } else if (event.type == 'switch_model_ack') {
      final choice = ModelChoice.fromJson(asJson(event.raw['choice']));
      model = ModelInfo(
        id: choice.id,
        name: choice.name,
        provider: choice.provider,
        reasoning: choice.reasoning,
      );
    } else if (event.type == 'session_deleted') {
      // 当前查看的 session 被删除（其他客户端删除 work 级联）：跳出视图
      if (asStr(event.raw['id']) == sessionId) {
        sessionId = null;
        items = [];
      }
    } else {
      changed = false;
    }
    if (changed) notifyListeners();
  }

  /// 拉取当前 session 快照（打开 session / 重连补齐用）。
  Future<void> _refreshSnapshot() async {
    final id = sessionId;
    if (id == null) return;
    try {
      final result = await _client.request(
        (rid) => ClientEvent.getState(id, rid),
      );
      final snapshot = asJson(result['snapshot']);
      items = messagesToItems(
        asJsonList(snapshot['messages']).map(Message.fromJson).toList(),
      );
      model = snapshot['model'] is Map
          ? ModelInfo.fromJson(asJson(snapshot['model']))
          : ModelInfo.placeholder;
      reasoning = asStrOrNull(snapshot['reasoning']);
      contextTokens = asInt(snapshot['context_tokens']);
      running = asBool(snapshot['running']);
      queue = asJsonList(snapshot['queue']).map(QueueEntry.fromJson).toList();
      project = asStrOrNull(snapshot['project']);
      parentSessionId = asStrOrNull(snapshot['parent_session_id']);
      question = PendingQuestion.fromJson(snapshot['pending_question']);
      goal = asStrOrNull(snapshot['goal']);
      stats = SessionStats.fromJson(snapshot);
      error = null;
    } on ServerException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  @override
  void dispose() {
    unawaited(_subscription.cancel());
    unawaited(_client.dispose());
    super.dispose();
  }
}

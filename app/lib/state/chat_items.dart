/// 聊天状态模型：把 agent 事件流（含历史快照）规整为可渲染的消息项列表。
///
/// 语义移植自原 web 前端 `lib/chat.ts`（同一事件协议，ADR-0046）：
/// - 历史快照（会话快照的 messages）直接渲染；
/// - 运行中的增量事件驱动「流式项」更新，MessageEnd 定稿后用完整消息替换；
/// - 工具卡片由 ToolExecution* 事件驱动（live），历史中的 ToolResult 消息
///   直接渲染（resume 后无事件可回放，两者统一展示形态）。
///
/// `messagesToItems` / `applyAgentEvent` 为纯函数，可直接单测。
library;

import 'dart:math';

import '../protocol/events.dart';
import '../protocol/models.dart';

enum ToolStatus { running, done, error }

/// 渲染项：用户消息 / assistant 消息 / 工具卡片 / 系统提示（压缩等）。
sealed class ChatItem {
  ChatItem() : id = 'item-${_counter++}';

  /// 渲染 key（MessageEnd 定稿时保留流式项的 id，避免列表重建闪烁）
  String id;

  static int _counter = 0;
}

class UserItem extends ChatItem {
  UserItem({
    required this.text,
    required this.imageCount,
    required this.timestamp,
  });

  final String text;
  final int imageCount;
  final int timestamp;
}

class AssistantItem extends ChatItem {
  AssistantItem({
    this.text = '',
    this.thinking = '',
    this.blocks = const [],
    this.streaming = false,
    this.streamPhase,
    this.stopReason,
    this.errorMessage,
    this.model,
    this.usage,
  });

  String text;
  String thinking;

  /// 最终内容块（MessageEnd 后权威）
  List<ContentBlock> blocks;
  bool streaming;

  /// 流式中最近一个内容块的种类（运行状态提示的阶段推导用）
  String? streamPhase;
  String? stopReason;
  String? errorMessage;
  String? model;
  Usage? usage;

  /// 空 assistant 消息（无正文、无思考、非失败、非流式）不渲染。
  bool get isEmpty {
    final failed = stopReason == 'error' || stopReason == 'aborted';
    return !failed && !streaming && text.isEmpty && thinking.isEmpty;
  }
}

class ToolItem extends ChatItem {
  ToolItem({
    required this.toolCallId,
    required this.name,
    this.args = const {},
    this.status = ToolStatus.running,
    this.resultPreview = '',
    this.isError = false,
  });

  final String toolCallId;
  String name;
  Json args;
  ToolStatus status;

  /// 结果文本预览（纯文本块拼接）
  String resultPreview;
  bool isError;

  /// 客户端接收时间戳（Unix 毫秒；仅 live 事件有，历史快照为 null，
  /// 执行过程卡片的步骤耗时据此计算）。
  int? startedAt;
  int? endedAt;

  Duration? get duration {
    final start = startedAt;
    final end = endedAt;
    if (start == null || end == null) return null;
    return Duration(milliseconds: end - start);
  }
}

class SystemItem extends ChatItem {
  SystemItem(this.text);

  final String text;
}

/// 执行过程分组（DESIGN.md「Tool ledger」）：连续的工具调用与纯思考段
/// 折叠为一张卡片，默认只露一行摘要（执行过程 · N 步 · 搜索 X 次 ·
/// 命令 Y 条 · 用时 Zs），展开才是逐步时间线。
class ExecutionItem extends ChatItem {
  ExecutionItem(List<ChatItem> steps) : steps = List.unmodifiable(steps) {
    // 组 id 锚定首个工具调用：流式增长（组变长）时渲染 key 稳定，
    // 展开态不丢
    id = 'exec-${tools.first.toolCallId}';
  }

  /// 步骤：ToolItem 或纯思考 AssistantItem（text 为空、thinking 非空）。
  final List<ChatItem> steps;

  List<ToolItem> get tools => steps.whereType<ToolItem>().toList();

  bool get hasRunning => tools.any((t) => t.status == ToolStatus.running);

  int get errorCount => tools.where((t) => t.status == ToolStatus.error).length;

  /// 整组用时（首个已开始的步骤到最后结束的步骤；历史快照无时间戳
  /// 时为 null，摘要不展示用时）。
  Duration? get elapsed {
    final starts = tools.map((t) => t.startedAt).nonNulls;
    final ends = tools.map((t) => t.endedAt).nonNulls;
    if (starts.isEmpty || ends.isEmpty) return null;
    return Duration(milliseconds: ends.reduce(max) - starts.reduce(min));
  }
}

/// 把连续的工具调用段（含夹在其中的纯思考段，≥2 步）折叠为
/// ExecutionItem；其余项与顺序不变。
List<ChatItem> groupExecutionSteps(List<ChatItem> items) {
  bool isStep(ChatItem item) =>
      item is ToolItem ||
      (item is AssistantItem && item.text.isEmpty && item.thinking.isNotEmpty);

  final out = <ChatItem>[];
  var i = 0;
  while (i < items.length) {
    if (!isStep(items[i])) {
      out.add(items[i]);
      i++;
      continue;
    }
    var j = i;
    while (j < items.length && isStep(items[j])) {
      j++;
    }
    final segment = items.sublist(i, j);
    final toolCount = segment.whereType<ToolItem>().length;
    if (segment.length >= 2 && toolCount >= 1) {
      out.add(ExecutionItem(segment));
    } else {
      out.addAll(segment);
    }
    i = j;
  }
  return out;
}

/// 历史消息 → 消息项（会话快照与 resume 用）。
List<ChatItem> messagesToItems(List<Message> messages) {
  final items = <ChatItem>[];
  for (final message in messages) {
    switch (message.role) {
      case 'user':
        items.add(
          UserItem(
            text: message.userText,
            imageCount: message.userImages.length,
            timestamp: message.timestamp,
          ),
        );
      case 'assistant':
        items.add(
          AssistantItem(
            text: message.assistantText,
            thinking: message.assistantThinking,
            blocks: message.blocks,
            stopReason: message.stopReason,
            errorMessage: message.errorMessage,
            model: message.model,
            usage: message.usage,
          ),
        );
        // 从历史 assistant 消息中的 tool_call 块恢复工具调用参数，
        // 确保 resume 后工具卡片仍能显示参数。
        for (final block in message.blocks) {
          if (block.type != 'tool_call') continue;
          if (_findToolIndex(items, block.toolCallId) >= 0) continue;
          items.add(
            ToolItem(
              toolCallId: block.toolCallId,
              name: block.toolName,
              args: block.arguments,
            ),
          );
        }
      case 'tool_result':
        _upsertToolResult(items, message);
    }
  }
  return items;
}

int _findToolIndex(List<ChatItem> items, String toolCallId) => items.indexWhere(
  (item) => item is ToolItem && item.toolCallId == toolCallId,
);

int _findStreamingAssistant(List<ChatItem> items) {
  for (var i = items.length - 1; i >= 0; i--) {
    final item = items[i];
    if (item is AssistantItem && item.streaming) return i;
  }
  return -1;
}

void _upsertToolResult(List<ChatItem> items, Message message) {
  final index = _findToolIndex(items, message.toolCallId);
  final status = message.isError ? ToolStatus.error : ToolStatus.done;
  if (index >= 0) {
    final tool = items[index] as ToolItem;
    tool.status = status;
    tool.resultPreview = message.resultText;
    tool.isError = message.isError;
    return;
  }
  items.add(
    ToolItem(
      toolCallId: message.toolCallId,
      name: message.toolName,
      status: status,
      resultPreview: message.resultText,
      isError: message.isError,
    ),
  );
}

/// 应用一个 agent 生命周期事件到消息项列表（原地修改 `items`）。
///
/// 返回权威上下文 token 估算（MessageEnd / AgentEnd / CompactionEnd 携带；
/// 其余事件返回 null，调用方不更新）。
int? applyAgentEvent(List<ChatItem> items, Object? event) {
  final kind = eventKind(event);
  final payload = eventPayload(event);
  switch (kind) {
    case 'AgentStart':
    case 'TurnStart':
    case 'TurnEnd':
      return null;
    case 'AgentEnd':
      return asIntOrNull(payload['context_tokens']);

    case 'MessageStart':
      final message = Message.fromJson(payload);
      switch (message.role) {
        case 'user':
          items.add(
            UserItem(
              text: message.userText,
              imageCount: message.userImages.length,
              timestamp: message.timestamp,
            ),
          );
        case 'assistant':
          items.add(AssistantItem(streaming: true));
        case 'tool_result':
          _upsertToolResult(items, message);
      }

    case 'MessageUpdate':
      _applyStreamDelta(items, payload);

    case 'MessageEnd':
      final message = Message.fromJson(asJson(payload['message']));
      if (message.isAssistant) {
        final index = _findStreamingAssistant(items);
        final finalized = AssistantItem(
          text: message.assistantText,
          thinking: message.assistantThinking,
          blocks: message.blocks,
          stopReason: message.stopReason,
          errorMessage: message.errorMessage,
          model: message.model,
          usage: message.usage,
        );
        if (index >= 0) {
          finalized.id = items[index].id;
          items[index] = finalized;
        } else {
          items.add(finalized);
        }
      } else if (message.isToolResult) {
        _upsertToolResult(items, message);
      }
      return asIntOrNull(payload['context_tokens']);

    case 'CompactionStart':
      items.add(SystemItem('正在压缩上下文…'));
    case 'CompactionEnd':
      items.add(
        SystemItem(
          '上下文已压缩（${asInt(payload['tokens_before'])} → '
          '${asInt(payload['context_tokens'])} tokens）',
        ),
      );
      return asIntOrNull(payload['context_tokens']);

    case 'ToolExecutionStart':
      final toolCallId = asStr(payload['tool_call_id']);
      final index = _findToolIndex(items, toolCallId);
      if (index >= 0) {
        final tool = items[index] as ToolItem;
        tool.name = asStr(payload['tool_name']);
        tool.args = asJson(payload['args']);
        tool.status = ToolStatus.running;
        tool.startedAt = DateTime.now().millisecondsSinceEpoch;
      } else {
        items.add(
          ToolItem(
            toolCallId: toolCallId,
            name: asStr(payload['tool_name']),
            args: asJson(payload['args']),
          )..startedAt = DateTime.now().millisecondsSinceEpoch,
        );
      }

    case 'ToolExecutionUpdate':
      final toolCallId = asStr(payload['tool_call_id']);
      final index = _findToolIndex(items, toolCallId);
      if (index >= 0) {
        final tool = items[index] as ToolItem;
        tool.resultPreview = _contentText(
          asJson(payload['partial'])['content'],
        );
      }

    case 'ToolExecutionEnd':
      final toolCallId = asStr(payload['tool_call_id']);
      final result = asJson(payload['result']);
      final isError = asBool(payload['is_error']);
      final index = _findToolIndex(items, toolCallId);
      if (index >= 0) {
        final tool = items[index] as ToolItem;
        tool.status = isError ? ToolStatus.error : ToolStatus.done;
        tool.resultPreview = _contentText(result['content']);
        tool.isError = isError;
        tool.endedAt = DateTime.now().millisecondsSinceEpoch;
      } else {
        items.add(
          ToolItem(
            toolCallId: toolCallId,
            name: asStr(payload['tool_name']),
            status: isError ? ToolStatus.error : ToolStatus.done,
            resultPreview: _contentText(result['content']),
            isError: isError,
          ),
        );
      }
  }
  return null;
}

/// 流式增量（AssistantEvent，外部标签形式）：TextDelta / ThinkingDelta 累积到
/// 当前流式 assistant 项；项缺失且收到 Start/TextDelta 时补建（与原前端一致）。
void _applyStreamDelta(List<ChatItem> items, Json event) {
  final kind = eventKind(event);
  final detail = eventPayload(event);
  var index = _findStreamingAssistant(items);
  if (index < 0 && (kind == 'Start' || kind == 'TextDelta')) {
    items.add(AssistantItem(streaming: true));
    index = items.length - 1;
  }
  if (index < 0) return;
  final current = items[index];
  if (current is! AssistantItem) return;
  switch (kind) {
    case 'TextStart':
      current.streamPhase = 'text';
    case 'ThinkingStart':
      current.streamPhase = 'thinking';
    case 'TextDelta':
      current.text += asStr(detail['delta']);
      current.streamPhase = 'text';
    case 'ThinkingDelta':
      current.thinking += asStr(detail['delta']);
      current.streamPhase = 'thinking';
  }
}

String _contentText(Object? content) {
  return asJsonList(
    content,
  ).where((b) => b['type'] == 'text').map((b) => asStr(b['text'])).join('\n');
}

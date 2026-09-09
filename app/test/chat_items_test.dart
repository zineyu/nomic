import 'package:nomic_app/protocol/models.dart';
import 'package:nomic_app/state/chat_items.dart';
import 'package:flutter_test/flutter_test.dart';

Message userMessage(String text) =>
    Message.fromJson({'role': 'user', 'content': text, 'timestamp': 1});

Message assistantMessage(List<Json> content) => Message.fromJson({
  'role': 'assistant',
  'content': content,
  'model': 'm',
  'stop_reason': 'stop',
  'usage': {'input': 1, 'output': 1, 'total_tokens': 2},
  'timestamp': 2,
});

void main() {
  group('messagesToItems（历史快照）', () {
    test('user + assistant + tool_result 规整为消息项', () {
      final items = messagesToItems([
        userMessage('ls 一下'),
        assistantMessage([
          {'type': 'text', 'text': '好的'},
          {
            'type': 'tool_call',
            'id': 'c1',
            'name': 'bash',
            'arguments': {'command': 'ls'},
          },
        ]),
        Message.fromJson({
          'role': 'tool_result',
          'tool_call_id': 'c1',
          'tool_name': 'bash',
          'content': [
            {'type': 'text', 'text': 'a.txt'},
          ],
          'is_error': false,
          'timestamp': 3,
        }),
      ]);
      expect(items[0], isA<UserItem>());
      expect(items[1], isA<AssistantItem>());
      // 历史 tool_call 块恢复工具卡片，tool_result 回填状态
      final tool = items[2] as ToolItem;
      expect(tool.toolCallId, 'c1');
      expect(tool.status, ToolStatus.done);
      expect(tool.resultPreview, 'a.txt');
      expect(tool.args, {'command': 'ls'});
    });

    test('孤儿 tool_result（无对应 tool_call）也渲染卡片', () {
      final items = messagesToItems([
        Message.fromJson({
          'role': 'tool_result',
          'tool_call_id': 'c9',
          'tool_name': 'read',
          'content': [
            {'type': 'text', 'text': 'x'},
          ],
          'is_error': true,
          'timestamp': 3,
        }),
      ]);
      final tool = items.single as ToolItem;
      expect(tool.status, ToolStatus.error);
    });
  });

  group('applyAgentEvent（流式）', () {
    test('MessageStart(user) 追加用户气泡', () {
      final items = <ChatItem>[];
      applyAgentEvent(items, {
        'MessageStart': {'role': 'user', 'content': 'hi', 'timestamp': 1},
      });
      expect(items.single, isA<UserItem>());
      expect((items.single as UserItem).text, 'hi');
    });

    test('TextDelta 累积到流式 assistant，MessageEnd 定稿并保留 id', () {
      final items = <ChatItem>[];
      applyAgentEvent(items, 'AgentStart');
      applyAgentEvent(items, {
        'MessageStart': {'role': 'assistant'},
      });
      applyAgentEvent(items, {
        'MessageUpdate': {
          'TextDelta': {'index': 0, 'delta': 'Hello'},
        },
      });
      applyAgentEvent(items, {
        'MessageUpdate': {
          'TextDelta': {'index': 0, 'delta': ' world'},
        },
      });
      final streaming = items.single as AssistantItem;
      expect(streaming.text, 'Hello world');
      expect(streaming.streaming, isTrue);

      final tokens = applyAgentEvent(items, {
        'MessageEnd': {
          'message': {
            'role': 'assistant',
            'content': [
              {'type': 'text', 'text': 'Hello world'},
            ],
            'model': 'm',
            'stop_reason': 'stop',
            'usage': {'input': 1, 'output': 1, 'total_tokens': 2},
            'timestamp': 9,
          },
          'context_tokens': 123,
        },
      });
      expect(tokens, 123);
      final finalized = items.single as AssistantItem;
      expect(finalized.streaming, isFalse);
      expect(finalized.text, 'Hello world');
      expect(finalized.id, streaming.id, reason: '定稿替换保留流式项 id');
    });

    test('MessageUpdate 无流式项时收到 TextDelta 自动补建', () {
      final items = <ChatItem>[];
      applyAgentEvent(items, {
        'MessageUpdate': {
          'TextDelta': {'index': 0, 'delta': 'x'},
        },
      });
      expect(items.single, isA<AssistantItem>());
    });

    test('ToolExecutionStart/End 驱动工具卡片状态', () {
      final items = <ChatItem>[];
      applyAgentEvent(items, {
        'ToolExecutionStart': {
          'tool_call_id': 'c1',
          'tool_name': 'bash',
          'args': {'command': 'ls'},
        },
      });
      var tool = items.single as ToolItem;
      expect(tool.status, ToolStatus.running);

      applyAgentEvent(items, {
        'ToolExecutionEnd': {
          'tool_call_id': 'c1',
          'tool_name': 'bash',
          'result': {
            'content': [
              {'type': 'text', 'text': 'ok'},
            ],
            'terminate': false,
          },
          'is_error': false,
        },
      });
      tool = items.single as ToolItem;
      expect(tool.status, ToolStatus.done);
      expect(tool.resultPreview, 'ok');
    });

    test('压缩事件追加系统项并携带权威 token 数', () {
      final items = <ChatItem>[];
      applyAgentEvent(items, {
        'CompactionStart': {'tokens_before': 100},
      });
      final tokens = applyAgentEvent(items, {
        'CompactionEnd': {
          'summary': 's',
          'tokens_before': 100,
          'context_tokens': 40,
          'kept_count': 2,
          'usage': {'input': 1, 'output': 1, 'total_tokens': 2},
        },
      });
      expect(items.length, 2);
      expect(items.every((i) => i is SystemItem), isTrue);
      expect(tokens, 40);
    });
  });
}

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

  group('groupExecutionSteps（执行过程折叠）', () {
    ToolItem tool(String id, {String name = 'bash'}) =>
        ToolItem(toolCallId: id, name: name);

    test('text 出现时折叠其前的执行段（含单条工具），组 id 锚定首个工具调用', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        AssistantItem(text: '第一段'),
        tool('c2', name: 'read'),
        tool('c3'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped.length, 5);
      // 单条工具段同样被 text 收尾折叠
      final first = grouped[1] as ExecutionItem;
      expect(first.tools.map((t) => t.toolCallId), ['c1']);
      expect(first.id, 'exec-c1');
      final second = grouped[3] as ExecutionItem;
      expect(second.tools.map((t) => t.toolCallId), ['c2', 'c3']);
    });

    test('夹在工具段中的纯思考段并入同组', () {
      final items = <ChatItem>[
        tool('c1'),
        AssistantItem(thinking: '先想想'),
        tool('c2'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped, hasLength(2));
      final run = grouped[0] as ExecutionItem;
      expect(run.steps.length, 3);
      expect(run.steps[1], isA<AssistantItem>());
    });

    test('尚无 text 收尾的尾部执行段不折叠（运行中实时可见）', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        tool('c2'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped.length, 3);
      expect(grouped[1], isA<ToolItem>());
      expect(grouped[2], isA<ToolItem>());
    });

    test('text 后再出现的执行段不折叠，直到下一次 text 出现', () {
      final items = <ChatItem>[
        tool('c1'),
        AssistantItem(text: 'a'),
        tool('c2'),
        tool('c3'),
      ];
      // 尾部段（c2/c3）尚无 text 收尾，保持平铺
      var grouped = groupExecutionSteps(items);
      expect(grouped.length, 4);
      expect(grouped[0], isA<ExecutionItem>());
      expect(grouped[2], isA<ToolItem>());
      expect(grouped[3], isA<ToolItem>());

      // 下一段 text 出现后折叠
      items.add(AssistantItem(text: 'b'));
      grouped = groupExecutionSteps(items);
      expect(grouped.length, 4);
      expect(grouped[0], isA<ExecutionItem>());
      final second = grouped[2] as ExecutionItem;
      expect(second.tools.map((t) => t.toolCallId), ['c2', 'c3']);
    });

    test('无工具的纯思考段不折叠（跟随的 text 只是叙述）', () {
      final items = <ChatItem>[
        AssistantItem(thinking: '先想想'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped, hasLength(2));
      expect(grouped[0], isA<AssistantItem>());
      expect(grouped[1], isA<AssistantItem>());
    });

    test('运行中 / 失败 / 用时从成员聚合', () {
      final running = tool('c1')..startedAt = 1000;
      final error = tool('c2')
        ..status = ToolStatus.error
        ..startedAt = 1000
        ..endedAt = 2500;
      final done = tool('c3')
        ..status = ToolStatus.done
        ..startedAt = 2500
        ..endedAt = 4000;
      final run = ExecutionItem([running, error, done]);
      expect(run.hasRunning, isTrue);
      expect(run.errorCount, 1);
      expect(run.elapsed, const Duration(milliseconds: 3000));
      expect(done.duration, const Duration(milliseconds: 1500));
    });
  });
}

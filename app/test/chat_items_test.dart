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

    test('被边界包裹的多步段折叠，组 id 锚定首个工具调用', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        tool('c2', name: 'read'),
        AssistantItem(text: '第一段'),
        tool('c3', name: 'read'),
        tool('c4'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped.length, 5);
      final first = grouped[1] as ExecutionItem;
      expect(first.tools.map((t) => t.toolCallId), ['c1', 'c2']);
      expect(first.id, 'exec-c1');
      final second = grouped[3] as ExecutionItem;
      expect(second.tools.map((t) => t.toolCallId), ['c3', 'c4']);
    });

    test('只有单个步骤的段不折叠（折叠没有收益）', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped.length, 3);
      expect(grouped[1], isA<ToolItem>());
    });

    test('用户输入也能收尾折叠（出错/中断后的步骤段不再永远平铺）', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        tool('c2'),
        UserItem(text: '换个问题', imageCount: 0, timestamp: 2),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped.length, 3);
      final run = grouped[1] as ExecutionItem;
      expect(run.tools.map((t) => t.toolCallId), ['c1', 'c2']);
      expect(grouped[2], isA<UserItem>());
    });

    test('出错/中止的 assistant 也是边界，能收尾折叠', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        tool('c2'),
        AssistantItem(stopReason: 'error', errorMessage: 'boom'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped.length, 3);
      expect(grouped[1], isA<ExecutionItem>());
      expect(grouped[2], isA<AssistantItem>());
    });

    test('SystemItem 透明：不打断步骤段、不参与边界判定，折叠时移到卡片后', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        tool('c2'),
        SystemItem('上下文已压缩'),
        tool('c3'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped.length, 4);
      final run = grouped[1] as ExecutionItem;
      expect(run.steps.length, 3, reason: '压缩提示不打断步骤段');
      expect(grouped[2], isA<SystemItem>(), reason: '系统提示移到卡片之后');
      expect(grouped[3], isA<AssistantItem>());
    });

    test('夹在工具段中的纯思考段并入同组', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        AssistantItem(thinking: '先想想'),
        tool('c2'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped, hasLength(3));
      final run = grouped[1] as ExecutionItem;
      expect(run.steps.length, 3);
      expect(run.steps[1], isA<AssistantItem>());
    });

    test('被边界包裹的纯思考段也折叠，组 id 锚定首个步骤', () {
      final thinking1 = AssistantItem(thinking: '先想想');
      final thinking2 = AssistantItem(thinking: '再想想');
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        thinking1,
        thinking2,
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped, hasLength(3));
      final run = grouped[1] as ExecutionItem;
      expect(run.steps, [thinking1, thinking2]);
      expect(run.tools, isEmpty);
      expect(run.id, 'exec-${thinking1.id}');
    });

    test('单个纯思考段不折叠（步数不足）', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        AssistantItem(thinking: '先想想'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped, hasLength(3));
      expect(grouped[1], isA<AssistantItem>());
    });

    test('尚无边界收尾的尾部执行段不折叠（运行中实时可见）', () {
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

    test('段首无边界（列表开头）不折叠', () {
      final items = <ChatItem>[
        tool('c1'),
        tool('c2'),
        AssistantItem(text: 'done'),
      ];
      final grouped = groupExecutionSteps(items);
      expect(grouped.length, 3);
      expect(grouped[0], isA<ToolItem>());
      expect(grouped[1], isA<ToolItem>());
    });

    test('text 后再出现的执行段不折叠，直到下一个边界出现', () {
      final items = <ChatItem>[
        UserItem(text: 'hi', imageCount: 0, timestamp: 1),
        tool('c1'),
        tool('c2'),
        AssistantItem(text: 'a'),
        tool('c3'),
        tool('c4'),
      ];
      // 尾部段（c3/c4）尚无边界收尾，保持平铺
      var grouped = groupExecutionSteps(items);
      expect(grouped.length, 5);
      expect(grouped[1], isA<ExecutionItem>());
      expect(grouped[3], isA<ToolItem>());
      expect(grouped[4], isA<ToolItem>());

      // 下一个边界（用户输入）出现后折叠
      items.add(UserItem(text: '继续', imageCount: 0, timestamp: 2));
      grouped = groupExecutionSteps(items);
      expect(grouped.length, 5);
      expect(grouped[1], isA<ExecutionItem>());
      final second = grouped[3] as ExecutionItem;
      expect(second.tools.map((t) => t.toolCallId), ['c3', 'c4']);
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

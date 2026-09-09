import 'package:nomic_app/protocol/events.dart';
import 'package:nomic_app/protocol/models.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('eventKind / eventPayload（外部标签枚举）', () {
    test('unit 变体为字符串', () {
      expect(eventKind('AgentStart'), 'AgentStart');
      expect(eventPayload('AgentStart'), isEmpty);
    });

    test('带负载变体为单键 map', () {
      final event = {
        'MessageEnd': {
          'message': {'role': 'user', 'content': 'hi', 'timestamp': 1},
          'context_tokens': 42,
        },
      };
      expect(eventKind(event), 'MessageEnd');
      expect(eventPayload(event)['context_tokens'], 42);
    });
  });

  group('Message 解析', () {
    test('user 消息 content 为字符串', () {
      final message = Message.fromJson({
        'role': 'user',
        'content': 'hello',
        'timestamp': 1,
      });
      expect(message.isUser, isTrue);
      expect(message.userText, 'hello');
      expect(message.userImages, isEmpty);
    });

    test('user 消息 content 为块数组（text + image）', () {
      final message = Message.fromJson({
        'role': 'user',
        'content': [
          {'type': 'text', 'text': 'look'},
          {'type': 'image', 'data': 'base64', 'mime_type': 'image/png'},
        ],
        'timestamp': 1,
      });
      expect(message.userText, 'look');
      expect(message.userImages.length, 1);
    });

    test('assistant 消息提取文本与思考', () {
      final message = Message.fromJson({
        'role': 'assistant',
        'content': [
          {'type': 'thinking', 'thinking': 'hmm'},
          {'type': 'text', 'text': 'answer'},
          {
            'type': 'tool_call',
            'id': 'c1',
            'name': 'bash',
            'arguments': {'command': 'ls'},
          },
        ],
        'model': 'claude',
        'stop_reason': 'stop',
        'usage': {'input': 1, 'output': 2, 'total_tokens': 3},
        'timestamp': 1,
      });
      expect(message.assistantText, 'answer');
      expect(message.assistantThinking, 'hmm');
      expect(message.blocks.length, 3);
      expect(message.usage?.totalTokens, 3);
    });

    test('tool_result 提取结果文本', () {
      final message = Message.fromJson({
        'role': 'tool_result',
        'tool_call_id': 'c1',
        'tool_name': 'bash',
        'content': [
          {'type': 'text', 'text': 'file.txt'},
        ],
        'is_error': false,
        'timestamp': 1,
      });
      expect(message.isToolResult, isTrue);
      expect(message.resultText, 'file.txt');
    });
  });

  group('摘要模型', () {
    test('WorkSummary 缺省标题回退', () {
      final work = WorkSummary.fromJson({
        'id': 'w1',
        'main_session_id': 's1',
        'title': null,
        'project': '/tmp/x',
        'session_count': 1,
        'message_count': 0,
        'last_message_at': null,
      });
      expect(work.displayTitle, '新会话');
    });

    test('PendingQuestion 解析元组形式（Rust serde）', () {
      final pending = PendingQuestion.fromJson([
        'q1',
        {
          'question': '继续？',
          'kind': 'single_choice',
          'options': ['是', '否'],
        },
      ]);
      expect(pending?.id, 'q1');
      expect(pending?.question.kind, 'single_choice');
      expect(pending?.question.options, ['是', '否']);
    });
  });

  group('ClientEvent 构造', () {
    test('prompt 携带 session_id 与 text', () {
      final event = ClientEvent.prompt('s1', 'hi');
      expect(event['type'], 'prompt');
      expect(event['session_id'], 's1');
    });

    test('answer_question 的 custom 仅在非空时携带', () {
      final withCustom = ClientEvent.answerQuestion('s1', 'q1', [
        'a',
      ], custom: 'x');
      expect(withCustom['custom'], 'x');
      final without = ClientEvent.answerQuestion('s1', 'q1', ['a']);
      expect(without.containsKey('custom'), isFalse);
    });
  });
}

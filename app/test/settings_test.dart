import 'dart:convert';

import 'package:nomic_app/protocol/events.dart';
import 'package:nomic_app/protocol/models.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('设置模型解析（get_settings 快照）', () {
    test('ProviderView：api_key 脱敏为 has_api_key', () {
      final provider = ProviderView.fromJson({
        'name': 'deepseek',
        'api': 'open_ai_completions',
        'base_url': 'https://api.deepseek.com/v1',
        'has_api_key': true,
        'updated_at': 1700000000000,
      });
      expect(provider.name, 'deepseek');
      expect(provider.api, 'open_ai_completions');
      expect(provider.baseUrl, 'https://api.deepseek.com/v1');
      expect(provider.hasApiKey, isTrue);
    });

    test('ProviderView：缺省字段回退（api 按名推断）', () {
      final provider = ProviderView.fromJson({
        'name': 'anthropic',
        'has_api_key': false,
        'updated_at': 0,
      });
      expect(provider.api, isNull);
      expect(provider.baseUrl, isNull);
      expect(provider.hasApiKey, isFalse);
    });

    test('ModelSpecRow：spec 字段 flatten 进同一 JSON 对象', () {
      final row = ModelSpecRow.fromJson({
        'provider': 'openai',
        'model_id': 'gpt-5.2',
        'name': 'GPT-5.2',
        'reasoning': true,
        'context_window': 400000,
        'cost_input': 1.75,
        'updated_at': 1700000000000,
      });
      expect(row.spec, 'openai/gpt-5.2');
      expect(row.displayName, 'GPT-5.2');
      expect(row.reasoning, isTrue);
      expect(row.vision, isNull);
      expect(row.contextWindow, 400000);
      expect(row.maxTokens, isNull);
      expect(row.costInput, 1.75);
      expect(row.summary, contains('ctx 400000'));
      expect(row.summary, isNot(contains('cache')));
    });

    test('SettingsSnapshot：标量读取与便捷 getter', () {
      final snapshot = SettingsSnapshot.fromJson({
        'providers': <Json>[],
        'model_specs': <Json>[],
        'settings': {
          'append_system': '保持简洁',
          'compaction.enabled': false,
          'prompts': ['a.md', 'b/'],
          'model_aliases': {'smart': 'openai/gpt-5.2'},
        },
        'scalar_keys': [
          'append_system',
          'prompts',
          'compaction.enabled',
          'model_aliases',
        ],
      });
      expect(snapshot.appendSystem, '保持简洁');
      expect(snapshot.compactionEnabled, isFalse);
      expect(snapshot.promptPaths, ['a.md', 'b/']);
      expect(snapshot.modelAliases, {'smart': 'openai/gpt-5.2'});
      expect(snapshot.isSet('append_system'), isTrue);
      expect(snapshot.isSet('no_such'), isFalse);
      expect(snapshot.scalarKeys, contains('model_aliases'));
    });

    test('SettingsSnapshot：空快照回退默认（压缩默认开启）', () {
      final snapshot = SettingsSnapshot.fromJson(const {});
      expect(snapshot.providers, isEmpty);
      expect(snapshot.compactionEnabled, isTrue);
      expect(snapshot.appendSystem, isEmpty);
      expect(snapshot.promptPaths, isEmpty);
      expect(snapshot.modelAliases, isEmpty);
    });
  });

  group('设置客户端事件', () {
    test('get_settings / set_setting / unset_setting 负载', () {
      expect(ClientEvent.getSettings('r1'), {
        'type': 'get_settings',
        'request_id': 'r1',
      });
      expect(ClientEvent.setSetting('r2', 'compaction.enabled', false), {
        'type': 'set_setting',
        'request_id': 'r2',
        'key': 'compaction.enabled',
        'value': false,
      });
      expect(ClientEvent.unsetSetting('r3', 'append_system'), {
        'type': 'unset_setting',
        'request_id': 'r3',
        'key': 'append_system',
      });
    });

    test('upsert_provider：patch 平铺进事件负载', () {
      final event = ClientEvent.upsertProvider('r4', 'deepseek', {
        'api': 'open_ai_completions',
        'base_url': 'https://api.deepseek.com/v1',
      });
      expect(event, {
        'type': 'upsert_provider',
        'request_id': 'r4',
        'name': 'deepseek',
        'api': 'open_ai_completions',
        'base_url': 'https://api.deepseek.com/v1',
      });
    });

    test('upsert_provider：patch 的 null 在 JSON 中保留（三态清除语义）', () {
      final event = ClientEvent.upsertProvider('r5', 'openai', {
        'api_key': null,
      });
      expect(event.containsKey('api_key'), isTrue);
      expect(event['api_key'], isNull);
      // jsonEncode 必须保留 null 字段（服务端据此区分「清除」与「不更新」）
      expect(jsonDecode(jsonEncode(event)), contains('api_key'));
    });

    test('upsert_model_spec / delete_model_spec 负载', () {
      final event = ClientEvent.upsertModelSpec('r6', 'openai', 'gpt-5.2', {
        'context_window': 400000,
      });
      expect(event, {
        'type': 'upsert_model_spec',
        'request_id': 'r6',
        'provider': 'openai',
        'model_id': 'gpt-5.2',
        'context_window': 400000,
      });
      expect(ClientEvent.deleteModelSpec('r7', 'openai', 'gpt-5.2'), {
        'type': 'delete_model_spec',
        'request_id': 'r7',
        'provider': 'openai',
        'model_id': 'gpt-5.2',
      });
    });
  });

  group('ServerEvent', () {
    test('settings_changed 广播识别', () {
      final event = ServerEvent.fromJson(const {'type': 'settings_changed'});
      expect(event.isSettingsChanged, isTrue);
      expect(event.sessionId, isEmpty);
    });
  });
}

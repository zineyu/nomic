/// WebSocket 事件流客户端（单例由 AppController 持有）。
///
/// 语义对齐原 web 前端 `createStreamClient`（ADR-0030/0046）：
/// - 断线自动指数退避重连（上限 15s）；重连成功后向事件流补发本地 `refresh`
///   事件（断开期间的事件已丢失，由控制层重新拉取快照补齐）。
/// - 查询类事件经 `request_id` 关联响应（30s 超时拒绝）；命令类为
///   fire-and-forget。
library;

import 'dart:async';
import 'dart:convert';

import 'package:web_socket_channel/web_socket_channel.dart';

import '../protocol/events.dart';
import '../protocol/models.dart';

/// 查询超时（与原 web 前端一致）。
const requestTimeout = Duration(seconds: 30);

/// 重连退避上限。
const maxRetryDelay = Duration(seconds: 15);

class WsClient {
  WsClient({required this.url});

  /// `ws://{host}/ws`。
  final String url;

  WebSocketChannel? _channel;
  StreamSubscription<dynamic>? _subscription;

  final _events = StreamController<ServerEvent>.broadcast();
  final _pending = <String, ({Completer<Json> completer, Timer timer})>{};
  final _connectWaiters = <Completer<void>>[];

  Timer? _retryTimer;
  int _retry = 0;
  bool _hasConnected = false;
  bool _disposed = false;

  /// 服务端事件流（重连后自动恢复；重连成功时补发本地 `refresh` 事件）。
  Stream<ServerEvent> get events => _events.stream;

  bool get connected => _channel != null;

  /// 确保已连接（幂等）；连接就绪后返回。
  Future<void> connect() {
    if (_channel != null) return Future.value();
    final waiter = Completer<void>();
    _connectWaiters.add(waiter);
    _connect();
    return waiter.future;
  }

  /// 发送 fire-and-forget 命令（连接未就绪时静默丢弃，与原前端一致）。
  void send(Json event) {
    _channel?.sink.add(jsonEncode(event));
  }

  /// 发送查询事件并等待携带同一 `request_id` 的响应（30s 超时）；
  /// 响应为 error 事件时抛出 [ServerException]。
  Future<Json> request(Json Function(String requestId) build) {
    final id = 'r${++_requestId}';
    final completer = Completer<Json>();
    final timer = Timer(requestTimeout, () {
      if (_pending.remove(id) != null && !completer.isCompleted) {
        completer.completeError(ServerException('请求超时: ${build(id)['type']}'));
      }
    });
    _pending[id] = (completer: completer, timer: timer);
    send(build(id));
    return completer.future;
  }

  int _requestId = 0;

  void _connect() {
    if (_disposed || _channel != null) return;
    final channel = WebSocketChannel.connect(Uri.parse(url));
    _channel = channel;
    _subscription = channel.stream.listen(
      _onMessage,
      onDone: _onClosed,
      onError: (_) => _onClosed(),
      cancelOnError: false,
    );
    // ready 在 WebSocket 握手完成后 resolve
    channel.ready
        .then((_) {
          if (_channel != channel) return;
          _retry = 0;
          for (final waiter in _connectWaiters) {
            if (!waiter.isCompleted) waiter.complete();
          }
          _connectWaiters.clear();
          // 重连（非首次连接）：事件流自动恢复，但断开期间的事件已丢失，
          // 通知控制层重新拉取快照
          if (_hasConnected) {
            _events.add(
              const ServerEvent(type: 'refresh', raw: {'type': 'refresh'}),
            );
          }
          _hasConnected = true;
        })
        .catchError((_) {
          // 握手失败：走 onDone/onError 的重连路径
        });
  }

  void _onMessage(dynamic data) {
    if (data is! String || data.isEmpty) return;
    Object? decoded;
    try {
      decoded = jsonDecode(data);
    } catch (_) {
      return;
    }
    if (decoded is! Map<String, dynamic>) return;
    final event = ServerEvent.fromJson(decoded);

    // 带 request_id 的响应事件 → 关联到 pending 请求
    final requestId = event.requestId;
    if (requestId.isNotEmpty) {
      final entry = _pending.remove(requestId);
      if (entry != null) {
        entry.timer.cancel();
        if (!entry.completer.isCompleted) {
          if (event.isError) {
            entry.completer.completeError(ServerException(event.message));
          } else {
            entry.completer.complete(event.raw);
          }
        }
        return;
      }
    }
    _events.add(event);
  }

  void _onClosed() {
    _subscription?.cancel();
    _subscription = null;
    _channel = null;
    _rejectAllPending('连接已断开');
    if (_disposed) return;
    _retry += 1;
    final delayMs = (1000 * (1 << _retry)).clamp(
      1000,
      maxRetryDelay.inMilliseconds,
    );
    _retryTimer?.cancel();
    _retryTimer = Timer(Duration(milliseconds: delayMs), _connect);
  }

  void _rejectAllPending(String reason) {
    for (final entry in _pending.values) {
      entry.timer.cancel();
      if (!entry.completer.isCompleted) {
        entry.completer.completeError(ServerException(reason));
      }
    }
    _pending.clear();
  }

  Future<void> dispose() async {
    _disposed = true;
    _retryTimer?.cancel();
    await _subscription?.cancel();
    await _channel?.sink.close();
    _channel = null;
    _rejectAllPending('连接已断开');
    await _events.close();
  }
}

/// 服务端返回的错误事件（error 响应携带的 message）。
class ServerException implements Exception {
  const ServerException(this.message);

  final String message;

  @override
  String toString() => message;
}

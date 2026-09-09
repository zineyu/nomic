# Nomic GUI（Flutter）

Flutter 桌面应用（macOS 优先），经 WebSocket 事件流连接 `nomic --serve`
（ADR-0046；协议见 ADR-0030）。

```bash
nomic --serve          # 终端 1：事件流服务（缺省 127.0.0.1:3333）
app-dev                # 终端 2（devenv）：flutter run -d macos
```

服务地址可用 `--dart-define=NOMIC_SERVE=ws://host:port/ws` 覆盖。

## 结构

- `lib/protocol/`：与服务端 serde JSON 对应的模型与事件（ClientEvent/ServerEvent）
- `lib/net/ws_client.dart`：WebSocket 客户端（退避重连、request_id 查询关联）
- `lib/state/chat_items.dart`：消息项 reducer（快照 + 事件增量合并，纯函数）
- `lib/app_controller.dart`：应用状态（ChangeNotifier）
- `lib/ui/`：界面（侧栏 / 聊天页 / 输入区 / 提问弹层 / 模型选择器）
- `lib/theme.dart`：视觉 token（与仓库根 `DESIGN.md` 同步）

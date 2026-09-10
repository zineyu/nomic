/// 系统文件选择器适配层：经 MethodChannel 调用 macOS NSOpenPanel
/// （实现见 `macos/Runner/AppDelegate.swift`；沙盒应用由 powerbox 完成
/// 授权，无需额外 entitlement）。第三方调用隔离在此薄适配器后。
library;

import 'package:flutter/services.dart';

/// 目录选择（登记 project 用）。
abstract final class FilePicker {
  static const _channel = MethodChannel('nomic/file_picker');

  /// 弹出系统目录选择面板；用户取消（或当前平台未实现）返回 `null`。
  static Future<String?> pickDirectory() async {
    try {
      final path = await _channel.invokeMethod<String>('pickDirectory');
      if (path == null || path.isEmpty) return null;
      return path;
    } on MissingPluginException {
      // 非 macOS 平台 / 测试环境未注册 handler
      return null;
    }
  }
}

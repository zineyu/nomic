import 'package:flutter/material.dart';

import 'app_controller.dart';
import 'theme.dart';
import 'ui/home_page.dart';

/// 事件流服务地址（`nomic --serve`；可用 --dart-define 覆盖）。
const serverUrl = String.fromEnvironment(
  'NOMIC_SERVE',
  defaultValue: 'ws://127.0.0.1:3333/ws',
);

void main() {
  runApp(NomicApp(controller: AppController(url: serverUrl)));
}

class NomicApp extends StatefulWidget {
  const NomicApp({super.key, required this.controller});

  final AppController controller;

  @override
  State<NomicApp> createState() => _NomicAppState();
}

class _NomicAppState extends State<NomicApp> {
  @override
  void initState() {
    super.initState();
    widget.controller.start();
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Nomic',
      debugShowCheckedModeBanner: false,
      theme: buildTheme(NomicTokens.light, dark: false),
      darkTheme: buildTheme(NomicTokens.dark, dark: true),
      home: HomePage(controller: widget.controller),
    );
  }
}

# ADR-0010: 移除内置 provider 与默认模型

## Status

Accepted

## Date

2026-08-06

## Context

启动路径长期内嵌两份硬编码知识（ADR-0004 的分层最底层「内置预设」）：

- **内置 provider 预设**：anthropic / openai 各自的默认模型 id
  （`claude-sonnet-4-5` / `gpt-5.2`）、特调规格（200k/64k/费率、400k/128k/零费率）
  与默认 base URL；
- **内置默认选择**：`ANTHROPIC_API_KEY` 环境推断 provider、回退链耗尽时落
  内置 provider 的默认模型；`/models` 候选列表固定把 anthropic / openai 排在最前。

问题：

- 默认模型 id 与规格随上游发布快速过期，硬编码值必然腐化（models.dev 目录
  与 `[providers.*.models]` 覆盖才是活数据）；
- 「静默选一个用户没要求过的模型」违背显式性：选错 provider 的代价
  （计费、数据出境）由用户承担；
- 两层默认（选择层 + 规格层）使优先级语义难以解释（ADR-0004 的大半篇幅在
  描述内置预设如何与其他层交互）。

模型选择迁移到 sqlite（ADR-0009）后，「记住用户上一次的选择」已覆盖
内置默认想解决的「免去每次指定」场景，内置默认失去存在理由。

## Decision

- 删除内置 provider 预设与内置默认 provider / 默认模型；`Preset` 只剩
  协议级中性兜底：按 API 种类的官方 base URL（anthropic_messages →
  api.anthropic.com，open_ai_completions → api.openai.com/v1）与全零规格。
- 模型选择按 **CLI 参数 > sqlite 配置回退链** 解析；两层都没有时启动报错，
  提示用 `--model <provider>/<模型id>` 指定。只给 `--provider` 不给模型同样
  报错（无默认模型）。
- provider 候选（`/models`）只列配置表 `[providers]` 定义的名字；
  当前模型的 provider 未在配置中定义时补入，保证当前模型始终可见。
- 保留按名推断 api（anthropic → anthropic_messages、openai →
  open_ai_completions）：这是命名便利而非内置 provider——不携带默认模型、
  特调规格或选择优先级。
- 模型存在性校验只剩两个权威来源：配置覆盖表与 models.dev 目录
  （目录不可用时维持降级不校验，同 ADR-0004）。

## Consequences

- 全新环境（无配置、无数据库选择）首次启动必须给 `--model`；之后经
  `/models` 切换并记住，体验不回归。
- 规格兜底从「貌似正确的特调值」变为「显然占位的中性零值」：上下文窗口等
  展示为 0 = 未知，避免拿过期数字当真。
- 离线行为变化：此前离线也能用内置预设跑默认模型，现在离线且数据库无
  选择时必须显式 `--model`（目录不可用时存在性校验降级，仍可启动）。
- 文档（README、config.example.toml）中所有「内置默认」表述同步为
  「协议默认 / 中性兜底」。

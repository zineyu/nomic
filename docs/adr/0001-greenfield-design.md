# ADR-0001: 全新设计，不保留旧实现资料

## Status

Accepted

## Context

nomic 仓库曾在历史版本中实现过一个功能完整的 TUI coding agent
（actor 模型、`nomic-core` / `nomic-ai` / `nomic-session` 三层、
47 篇 ADR），后被刻意重置回初始骨架状态。本次需求「实现一个 coding
agent，搭建初期脚手架」需要明确与旧代码库的关系。

## Decision

**全新设计**。不恢复、不归档任何旧代码与旧 ADR；git history 自然存在
但不作为设计输入。所有后续设计问题均以「如果没有旧代码，今天怎么选」
作答。

## Consequences

- 初期脚手架从零设计，不为兼容旧概念（session recorder、steering
  queue、actor 邮箱等）预留接口。
- 旧版踩过的坑不再通过文档传递，若需要经验教训只能查阅 git history。
- 设计空间完全开放，每个决策都需要重新论证。

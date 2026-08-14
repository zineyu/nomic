# ADR-0025: 聊天区几何上移状态层，渲染回写通道删除

## Status

Accepted

## Date

2026-08-15

## Context

聊天区的滚动与游标定位依赖各条目折行后的起始行与滚动上限，而这些
几何信息过去由渲染期回写：`ChatView`（`StatefulWidget`）渲染时自行
折行，经 `Chat::clamp_scroll` / `Chat::sync_item_lines` 把上限与起始行
写回 `Chat`。状态层文档自己承认「未经渲染时为空」——按键行为的结果
取决于上一帧是否画过，这条时序不变量不在任何 interface 里，只在注释
里。后果：

- 滚动/游标测试必须先渲一帧（或在测试里手工模拟回写，如
  `sync_item_lines(vec![0, 10, 20, 30, 40])` 这类与真实折行脱节的
  假数据）；
- 渲染层无法换成不同折行实现而不惊动状态层——seam 是假的，状态层
  的正确性寄生在渲染实现细节上。

## Decision

「宽度 → 折行 → 条目起始行」的几何计算上移到状态层，渲染前按已知
视口主动计算：

- 行组装（条目 → 带 gutter 的物理行 + 各条目起始行）抽为
  `tui/chat_lines.rs`，是几何的唯一实现；状态层
  （`Chat::sync_geometry`，由 `App::sync_chat_geometry(width, height)`
  携带 thinking 折叠与 spinner 帧调用）与渲染上屏共用同一函数，
  行数与起始行精确一致；
- `draw` 每帧在渲染聊天区前调用 `sync_chat_geometry(chat_area.width,
  chat_area.height)`：条目起始行与滚动上限写进 `Chat`，滚动偏移就地
  钳制；
- `ChatView` 退化为纯只读 `Widget`（持 `&App`），滚动偏移与上限直接
  读状态层；回写通道 `sync_item_lines` / `clamp_scroll` 删除。

游标整行高亮与搜索高亮只改样式、不改行数，几何计算传
`cursor: None` 即可，两处结果必然一致。

## Consequences

- 滚动与游标测试不再需要先渲一帧：`sync_chat_geometry(40, 5)` 后
  直接断言按键后的滚动位置；
- 时序不变量从注释进类型：`ChatView` 没有 `&mut Chat`，物理上无法
  回写；
- 折行实现可在 `chat_lines.rs` 一处替换（几何与上屏同时换），状态层
  不受影响——seam 变真实；
- 代价：行组装每帧两次（几何一次、上屏一次）。组装是纯函数且历史
  规模有限，先保持简单；若成瓶颈可加脏标记缓存，但单一实现保证
  两侧一致的原则不变。

HELP 弹层的滚动上限仍由渲染期钳制（`HelpOverlay` 自有通道），与本
决策无关，口径见 ADR-0019。

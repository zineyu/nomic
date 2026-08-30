# ADR-0038: 子 agent 模型继承与别名

- 状态：已接受
- 日期：2026-09-02

## 背景

ADR-0031 引入了多 agent supervisor，但 `create_agent` 的 `model` 参数为
必填项，且只接受裸模型 ID。实践中暴露出两个问题：

1. **样板负担**：多数子 agent 与主 agent 用同一模型即可，强制每次创建
   都从可用模型列表里抄一个模型 ID，徒增 token 与出错面（列表很长时
   LLM 常抄错）。
2. **选择靠猜**：模型 ID 不表达能力。给子任务挑模型时，主 agent 真正
   关心的是「这活要不要强推理」「要不要看图」，而不是某个具体 ID。
   用户也无法把自己常用的「强项模型 / 快速模型 / 多模态模型」固化成
   稳定的选择入口。

## 决策

### 模型三层解析

`create_agent` 的 `model` 参数改为**可选**，按三层解析：

1. **别名**：命中别名表（`config.toml` 的 `[model_aliases]`，别名 →
   `<provider>/<模型id>`）时使用对应模型；
2. **模型标识**：`<provider>/<模型id>` 全形式或裸模型 id 在可用模型
   列表中匹配（裸 id 跨 provider 歧义时报错并提示全形式）；
3. **继承**：参数缺省时继承主 agent 的**当前**模型。

解析收在 `AgentSupervisor::resolve_model`，工具层（`create_agent`）只做
「缺省 → 继承」的分派；`CreateAgentRequest::model` 仍为完整 `Model`
（supervisor 的创建契约不变）。

### 继承跟随运行期切换

继承的语义是「主 agent 的当前模型」，而非启动时的模型：主 agent 经
TUI `/models` 或 web `switch_model` 切换后，后续创建的子 agent 继承
新模型。载体是 nomic-core 的 `SharedModel`（`Arc<RwLock<Model>>` 共享
单元）：supervisor 在创建子 agent 时读，入口在切换成功时写
（TUI `ModelSwitcher::apply`、web `handle_switch_model`），经
`agent_recipe` 组装时建立并由 `AgentRecipe::inherited_model_cell`
交给入口持有。web 每个 session 独立一份（模型是会话级状态）。

### 别名按能力区分

别名由用户在 `[model_aliases]` 中配置（如 `smart` / `fast` / `vision`），
目标模型的规格与主模型同一分层口径（配置覆盖 > models.dev > 中性兜底）
在 bootstrap 解析为完整 `Model`；指向未知 provider 或格式非法时启动
硬报错（与配置文件校验同一口径）。

为支撑「按多模态能力区分」，`Model` / `ModelSpec` 新增 `vision` 字段
（是否支持图像输入）：models.dev 目录从 `modalities.input` 含 `image`
解析，配置可显式覆盖；`ModelSpec::is_complete` 随之扩为 9 字段（写全
后仍跳过 models.dev 加载）。

`create_agent` 的工具描述为每个别名与可用模型标注能力标签
（`[reasoning]` = 智力维度，`[vision]` = 多模态维度），LLM 按子任务的
能力需求选择别名；别名未配置时描述中相应区段标注未配置。

### 三入口同一口径

别名与继承的接线收在 `agent_recipe::assemble`（ADR-0032 的统一装配点），
TUI / print / web 三入口只负责提供 `default_model` 与 `model_aliases`，
行为天然一致；交互端额外持有共享单元以跟进运行期模型切换。

## 边界

- **子 agent 不继承 provider / api_key 等连接参数**：`provider` 仍按
  ADR-0031 走 supervisor 默认 provider，跨 provider 模型切换的连接
  重建与主 agent `/models` 同一口径；本 ADR 只改模型选择语义。
- **别名是创建期解析**：子 agent 创建后模型固定，主 agent 后续切换
  模型不影响已创建的子 agent。
- **别名配置不做存在性强校验**：目标模型在 models.dev 目录与配置覆盖
  表中都不存在时，与主模型解析同样降级为告警 + 中性兜底（离线目录
  不可用时无法校验），只有 provider 未知或格式非法才硬报错。

## 后果

- 常见路径零参数：子 agent 缺省继承主 agent 模型，且继承跟随运行期
  切换，TUI / web 行为一致。
- 用户可用别名将「按能力挑模型」固化为配置，LLM 经能力标签做出
  符合任务需求的选择，而非猜测模型 ID。
- `Model` 新增 `vision` 字段为后续多模态相关功能（如按能力过滤
  `--image` 可用模型）提供了数据基础。

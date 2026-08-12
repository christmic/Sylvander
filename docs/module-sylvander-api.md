# Module Reference — `sylvander-api`

> 公共服务线协议与 JSON Schema crate。
> Source: [`sylvander-api/src/`](../sylvander-api/src)

## 1. 是什么

`sylvander-api` 只拥有客户端、Channel 与 Runtime 跨服务边界交换的版本化
DTO、协议协商、纯校验和 JSON Schema。公开数据使用 `serde` 和
`schemars::JsonSchema`；它们是 TUI、桌面和其他语言客户端代码生成的依据。

它不拥有异步运行时、进程内消息总线、数据库、网络客户端、Agent 执行或 provider
协议。`MessageBus` 与默认的有界内存实现属于 `sylvander-channel`；Runtime 负责组合
应用端口和本 crate 的消息 DTO。

## 2. 为什么按领域拆分

曾经的 `types.rs` 同时包含协商、模型、平台、Session、消息和反馈类型，所有权不清，
也使新协议容易继续堆进一个公共文件。该集散模块已删除。每个领域现在同时拥有类型、
校验、模块文档与白盒测试；crate 根只做公共 DTO 的统一导出。

领域拆分不改变 serde wire shape、Schema 类型名或协议版本。内部调用者应使用明确的
领域路径，外部普通调用者可以使用 crate 根导出。

## 3. 领域模块

| 模块 | 所有内容 | 不包含 |
|---|---|---|
| `negotiation` | UI 版本范围、能力名、握手与失败 | 连接状态、兼容降级 |
| `identity` | `AgentId`、`SessionId`、`UserId` | 已认证执行身份 |
| `model` | provider-qualified 模型选择、能力、价格、推理级别 | provider wire 请求 |
| `platform` | 脱敏能力、命令和工具展示声明 | 凭据、回调、命令参数 |
| `session` | sparse overrides、有效配置、版本钉住、prompt manifest、workspace DTO | Session 生命周期和存储 |
| `message` | 消息信封、附件、流式事件、系统控制 | 发布、订阅和背压实现 |
| `execution` | 权限、上下文、压缩、回滚、超时和重试结果 DTO | 沙箱句柄和可执行权限 |
| `feedback` | opaque target 与证据引用 | Runtime run/turn 内部 ID |
| `boundary` | 已认证入口上下文和内容安全错误 | transport 凭据 |
| `ui` | 顶层客户端/服务端消息 | 监听器和处理器 |
| `agent_admin` | Agent 定义管理子协议 | Runtime Agent 定义实现 |
| `registry_admin` | Provider、Model、Credential 管理子协议 | provider 客户端和密钥值 |
| `identity_binding` | transport 身份绑定子协议 | transport 认证实现 |
| `user_profile` | owner-free User Profile 子协议 | Agent prompt snapshot |
| `memory_confirmation` | Guardian 记忆确认子协议 | 记忆存储 |
| `session_context` | 历史服务上下文 DTO | Agent `AgentExecutionContext` |
| `schema` | 当前协议 Schema 聚合函数 | 代码生成运行时 |

## 4. 运行时边界

```text
TUI / Desktop / external Channel
              |
              v
      protocol DTO + schema
              |
              v
Runtime authorization / Session service
              |
      +-------+--------+
      |                |
Channel MessageBus   AgentTurnRequest
(Rust app port)      (Agent domain)
```

典型请求流程：

1. Channel 建立可信 `BoundaryContext`；客户端不能自行声明认证结果。
2. transport 解码当前版本 `UiClientMessage` 并交给 Runtime `ChannelHost`。
3. Runtime 认证、授权并加载钉住的 Session/config 快照。
4. Runtime 把公共 DTO 显式投影成 Agent 领域输入，并把 Agent 事件投影回
   `StreamEvent`。
5. Runtime 完成持久化后，通过 Channel 层的 `MessageBus` 发布 DTO。

## 5. 约束

- 本 crate 的正常依赖不得包含 Tokio、async-trait、HTTP、数据库、Agent、Runtime 或
  provider crate。
- 新公开字段必须可序列化、可生成 Schema，并由当前版本信封约束。
- 未知字段、未知版本和旧 shape 默认失败关闭；禁止隐式 fallback 或双读写。
- Model 选择始终是 `(provider_id, model_id)`；裸 `model_id` 不是有效选择。
- 凭据、原始 prompt、内部 run/turn ID 和可执行 authority 不得进入公开 DTO。
- Rust `use` 必须位于模块作用域；不得在函数体内临时导入。

## 6. Schema 与测试

```bash
cargo run -p sylvander-api --example generate_ui_schema
cargo test -p sylvander-api
```

测试文件与领域一一对应：`identity.rs`、`model.rs`、`platform.rs`、`session.rs`、
`message.rs`、`feedback.rs`、`negotiation.rs`，以及各版本化子协议测试。`schema.rs`
验证顶层 Schema 仍覆盖当前 UI、管理、身份、Profile 和记忆确认契约。

## 7. 相关文档

- [`agent-runtime-api-boundaries.md`](agent-runtime-api-boundaries.md)
- [`product-module-architecture.md`](product-module-architecture.md)
- [`boundary-authorization.md`](boundary-authorization.md)
- [`identity-binding-protocol.md`](identity-binding-protocol.md)
- [`user-profile-protocol.md`](user-profile-protocol.md)
- [`server-configuration.md`](server-configuration.md)

# sylvander-agent

`sylvander-agent` 是 provider-neutral 的 Agent 执行内核：接收一个已经解析并冻结的
执行请求，调用 `sylvander-llm-core` 模型端口，执行受治理的工具，将工具结果回灌，
并返回完整执行结果。

## 它是什么

稳定的执行策略由 `AgentLoop` 表示。每次执行的易变数据和能力必须分别进入：

- `AgentTurnRequest`：会话快照、精确模型元数据、系统指令、推理参数、工具快照和
  可信执行身份；
- `AgentExecutionPorts`：模型提供者、工具环境、调用授权网关及交互门；
- `AgentOutcome`：更新后的对话、最终模型响应、迭代数和累计用量。

这种分离保证可复用的循环策略不会携带某次 Session 的模型、工具、工作区或权限，
也禁止把可反序列化的客户端请求直接变成可执行能力。

## 它不是什么

Agent 不是产品 Session、服务协议或基础设施组合根：

- Runtime 负责认证、Session 生命周期、模型与凭据解析、持久化、可观测性、沙箱和
  具体文件系统/进程实现；
- `sylvander-api` 只定义稳定的客户端线协议；
- provider crate 只负责官方协议的 wire 编解码；
- Channel/TUI/Desktop 通过 Runtime 的应用端口工作，不直接读取 Agent 存储。

Agent 只保留 workspace executor、变更日志和压缩产物的中立端口。host-local、SSH、
OCI、MCP stdio、SQLite、变更 manifest、崩溃恢复及文件系统产物适配器均由 Runtime
实现和选择。后续结构演进以
[`docs/agent-runtime-api-boundaries.md`](../docs/agent-runtime-api-boundaries.md) 为准。

Agent 不导出服务消息总线类型。Rust 应用总线契约由 `sylvander-channel` 持有，消息
负载才属于 `sylvander-api`；Runtime 负责组合两者。逻辑 workspace mount 使用 Agent
自己的 `WorkspaceCapabilities`，Runtime 必须从已认证的会话配置显式映射，不能把
可反序列化的 API 权限对象直接注入工具。Agent 的正常依赖图中，唯一第一方依赖是
`sylvander-llm-core`。

## 最小执行

```rust,no_run
use std::sync::Arc;

use sylvander_agent::{
    prelude::{
        AgentExecutionContext, AgentExecutionPorts, AgentLoop, AgentTurnRequest, ChatMessage,
        ConversationSnapshot, ToolContext, ToolRegistry,
    },
    tool_invocation::{RegistryBoundToolGateway, ToolInvocationGateway as _},
};
use sylvander_llm_core::{
    ModelCapabilities, ModelInfo, ModelProvider, ModelRef,
};

# fn model_provider() -> Arc<dyn ModelProvider> { unimplemented!() }
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let kernel = AgentLoop::builder()
    .max_iterations(50)
    .max_retries(3)
    .build();
let execution = AgentExecutionContext::restricted_for("user", "agent", "execution");
let tools = ToolRegistry::new();
let gateway = RegistryBoundToolGateway::new(tools.invocation_descriptors());
let request = AgentTurnRequest {
    conversation: ConversationSnapshot::new(vec![ChatMessage::user("Say hello")]),
    model: ModelInfo {
        reference: ModelRef::new("configured-provider", "selected-model"),
        context_window: 200_000,
        max_output_tokens: 32_000,
        capabilities: ModelCapabilities::empty(),
    },
    system_instructions: Vec::new(),
    reasoning: None,
    tools,
    execution: execution.clone(),
};
let ports = AgentExecutionPorts::new(
    model_provider(),
    ToolContext::new(execution),
    gateway.clone(),
    gateway.snapshot(),
);

let outcome = sylvander_agent::prelude::run(&kernel, request, ports).await?;
println!("finished after {} iterations", outcome.iterations);
# Ok(())
# }
```

`run_stream` 提供完整事件流；`run_with_events` 将非终止事件交给回调并返回同一个
`AgentOutcome`。终止成功由 `AgentEvent::Done(AgentOutcome)` 表示，失败由返回的
`AgentLoopError` 表示。

## 工具契约

工具定义与执行分离：

- `ToolDefinition::spec` 返回稳定、provider-neutral 的 JSON Schema 和执行策略；
- `ToolRegistry::prepare` 在授权前验证并规范化模型输入；
- `ToolInvocationGateway` 是所有可执行工具共同的授权与审计边界；
- `ToolExecutor::handle` 只接收不可变的 `PreparedToolCall` 和 Runtime 构造的
  `ToolContext`；
- 模型可见错误使用 `ToolOutput { is_error: true }`，系统致命错误使用 `ToolError`。

工具执行前，`AgentExecutionPorts::validate_for` 会验证请求身份、工具快照和网关的
可执行表面一致；不一致时在模型、hook 或工具工作开始前失败关闭。

进程工具必须声明强制沙箱要求。只有能证明文件系统隔离、默认拒绝网络及资源限制
均已生效的 executor 才能执行；本地或 SSH executor 不会冒充完整沙箱。Agent 本身
是可信控制平面并运行在沙箱外，只有工具进程进入沙箱数据平面。

Write/Edit 在写入前通过 `WorkspaceMutationJournal` 请求 Runtime 持久化回滚状态，
写入成功后提交不透明句柄。Agent 不解析 manifest，也不执行恢复。超大工具结果同样
只通过 `ToolResultDisk` 端口持久化；生产目录、生命周期和路径安全由 Runtime 决定。

## 循环语义

```text
Runtime freezes AgentTurnRequest + AgentExecutionPorts
  -> Agent validates model capabilities and execution authority
  -> compresses the model-visible conversation when required
  -> opens the exact provider-qualified model route
  -> emits bounded typed streaming events
  -> prepares, authorizes and executes tool calls
  -> re-feeds bounded tool results
  -> returns AgentOutcome to Runtime for atomic persistence
```

模型打开失败可按稳定策略重试；已产生可见流内容后不会自动重放。模型能力在 dispatch
前检查，工具、图片、文档、推理、结构化输出和 prompt cache 不支持时均失败关闭。

## 代码风格

- `use` 必须位于模块顶部；函数或代码块内部不引入普通 `use`，除非有明确、记录的
  特殊原因；
- 公共模块、类型和安全边界注释必须同时说明“是什么”和“为什么”；
- provider-specific 类型不得进入工具和 Agent 公共契约；
- 不保留无批准迁移方案的兼容 fallback；
- Runtime 独占身份、持久化、基础设施和凭据组合权。

## 验证

```bash
cargo test -p sylvander-agent --locked
cargo clippy -p sylvander-agent --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-agent --no-deps --locked
```

单元、契约、fixture provider 和选择性真实 provider 测试均位于 `tests/`。普通 CI 不
依赖真实凭据；真实协议测试必须显式提供对应 provider 环境变量。

更完整的产品分层、工具执行和沙箱设计见：

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- [`docs/agent-runtime-api-boundaries.md`](../docs/agent-runtime-api-boundaries.md)
- [`docs/product-module-architecture.md`](../docs/product-module-architecture.md)
- [`docs/tool-execution-architecture.md`](../docs/tool-execution-architecture.md)

## License

MIT

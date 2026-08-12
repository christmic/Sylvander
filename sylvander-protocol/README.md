# sylvander-protocol

`sylvander-protocol` 是 Sylvander 当前的公共服务线协议与 JSON Schema crate；目标名称
是 `sylvander-api`。它定义客户端、Channel 与 Runtime 跨进程交换的版本化 DTO，包含
请求、响应、事件、标识符、脱敏视图、协议协商及纯校验。

## 为什么保持纯数据

线协议需要可被 Rust 之外的客户端生成、验证和长期兼容。异步 trait、Tokio channel、
数据库句柄和进程内实现无法跨语言，也会把 Runtime 生命周期错误地伪装成 API。
因此本 crate 不包含消息总线、网络监听、存储、Agent 执行或 provider 客户端。

Rust 进程内的 `MessageBus`、订阅过滤、背压错误、诊断与默认内存实现由
`sylvander-channel` 持有；它们传递本 crate 的 `BusMessage`，但不是线协议的一部分。
Runtime 是唯一同时组合 Protocol DTO、Channel 应用端口和 Agent 执行的生产层。

## 领域模块

- `identity`：Agent、Session、User 的稳定公共标识；
- `message`：消息信封、附件、流式事件和系统控制 DTO；
- `feedback`：Runtime 证据绑定的用户评价 DTO；
- `model`：provider-qualified 模型目录、能力和推理级别；
- `platform`：脱敏平台能力与展示声明；
- `session`：Session 配置、版本钉住、prompt manifest 与 workspace DTO；
- `execution`：权限、进度和恢复结果，不含可执行 authority；
- `negotiation`：当前 UI 版本和能力协商；
- `ui`：客户端与服务端顶层消息；
- `agent_admin`、`registry_admin`、`identity_binding`、`user_profile`、
  `memory_confirmation`：各自独立版本化的服务子协议。

crate 根重新导出公共 DTO，供普通调用者使用；领域模块路径用于所有权清晰的内部实现
和文档。旧的 `types` 集散模块已经删除。拆分不得改变 serde 形状、Schema 名称或
协议版本。

## 依赖约束

- 允许 Serde、JSON Schema、UUID 和纯校验依赖；
- 禁止 Tokio、async-trait、HTTP/数据库客户端、Agent、Runtime 和 provider crate；
- 新的公开类型必须可序列化、可生成 Schema，并带版本或受现有版本信封约束；
- 错误与 Debug 输出不得暴露凭据、原始 prompt 或其他受保护内容。

生成当前 UI 协议 Schema：

```bash
cargo run -p sylvander-protocol --example generate_ui_schema
```

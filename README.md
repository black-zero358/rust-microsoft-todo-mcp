# rust-microsoft-todo-mcp

A Rust-based Microsoft To Do MCP server focused on complete task management, incremental synchronization, and efficient agent workflows.

**Status: M1 scope agreed; implementation pending.**

## 项目定位

面向个人自托管的远程 Microsoft To Do MCP 服务，部署在 VPS / Docker，供 ChatGPT 等客户端连接。使用 Rust 实现，以 [Softeria/ms-365-mcp-server](https://github.com/Softeria/ms-365-mcp-server) 的 To Do 能力为功能参考，优化实现并补全任务工作流。

## 已确认的方案

- Streamable HTTP 远程接入，提供 HTTPS 部署说明及客户端认证。
- 管理自己的一个或多个微软账户，支持默认账户及数据隔离。
- 本地持久化任务正文、搜索索引及同步状态；后台增量同步。
- 部署者自备 Microsoft 应用注册和 Client ID，提供逐步配置文档。
- 轻量管理页面：添加账户、查看同步状态、撤销授权。

## M1：首个可用版本（待实现）

- 清单与任务完整管理，包括正文、重要性、分类、截止日期、提醒和重复规则。
- 步骤／检查项及关联资源的增删改查。
- 中文标题与正文搜索，跨清单／跨账户搜索及结构化筛选。
- 首次全量同步、清单与任务 Delta、手动刷新、同步状态及中断恢复。
- Docker 与持久化卷、账户授权刷新、分页、限流退避、写入超时核对和日志脱敏。

完整范围、设计基线及验收场景见 **[M1 项目共识与验收标准](docs/M1-SPEC.md)**。

## M2

附件列出、上传、下载、删除、分块上传恢复，以及批量工作流和进一步的性能与恢复测试。

## 性能目标

测量启动时间、内存、搜索延迟、Graph 请求次数及工具响应体积；具体指标随测试环境和工作负载确定。

## References

- [Reference endpoint definitions](https://github.com/Softeria/ms-365-mcp-server/blob/main/src/endpoints.json)
- [Microsoft To Do API](https://learn.microsoft.com/en-us/graph/api/resources/todo-overview?view=graph-rest-1.0)
- [Task delta](https://learn.microsoft.com/en-us/graph/api/todotask-delta?view=graph-rest-1.0)
- [Linked resource update](https://learn.microsoft.com/en-us/graph/api/linkedresource-update?view=graph-rest-1.0)
- [Attachment upload session](https://learn.microsoft.com/en-us/graph/api/taskfileattachment-createuploadsession?view=graph-rest-1.0)

许可证待定。

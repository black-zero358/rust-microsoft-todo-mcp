# rust-microsoft-todo-mcp

A Rust-based Microsoft To Do MCP server focused on complete task management, incremental synchronization, and efficient agent workflows.

**Status: project planning.** Implementation and architecture decisions will follow scope agreement.

## 项目目标

使用 Rust 构建专注 Microsoft To Do 的高性能 MCP 服务。以 [Softeria/ms-365-mcp-server](https://github.com/Softeria/ms-365-mcp-server) 的 To Do 能力为功能参考，优化实现并补全任务工作流。

## 目标功能（待实现）

- 清单：列出、读取、创建、改名、删除，并遵循微软内置清单限制。
- 任务：创建、读取、修改、删除、完成、重新打开。
- 属性：正文、重要性、分类、截止日期、提醒、重复规则。
- 步骤：列出、新增、改名、勾选、取消勾选、删除。
- 关联资源：读取、新增、更新、删除。
- 附件：列出、上传、下载、删除，以及分块上传流程。
- 搜索：跨清单文本搜索与结构化筛选。
- 同步：清单和任务 Delta、持久化游标、后台同步与恢复。

## 待讨论的设计方向

- 部署：本地 stdio、远程 Streamable HTTP，以及首发优先级。
- 身份：个人 Microsoft 账户、工作/学校账户、单账户与多账户。
- 数据：按需直连 Graph，或 SQLite 缓存与增量同步；明确数据新鲜度。
- 工具：面向任务的稳定类型化接口、精简返回、分页与批量操作。
- 可靠性：限流退避、超时后的写入结果核对、同步恢复、时间与重复规则语义。
- 性能：测量启动时间、内存、延迟、Graph 请求数与响应体积。
- 发布：Windows / Linux / macOS 原生二进制、Docker；许可证待定。

## References

- [Reference endpoint definitions](https://github.com/Softeria/ms-365-mcp-server/blob/main/src/endpoints.json)
- [Microsoft To Do API](https://learn.microsoft.com/en-us/graph/api/resources/todo-overview?view=graph-rest-1.0)
- [Task delta](https://learn.microsoft.com/en-us/graph/api/todotask-delta?view=graph-rest-1.0)
- [Linked resource update](https://learn.microsoft.com/en-us/graph/api/linkedresource-update?view=graph-rest-1.0)
- [Attachment upload session](https://learn.microsoft.com/en-us/graph/api/taskfileattachment-createuploadsession?view=graph-rest-1.0)

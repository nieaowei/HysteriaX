# 0017 服务端文件按职责命名

实施日期：2026-11-05（Asia/Shanghai）。承接 0016 文件整理，按用户要求处理残留的 v2 文件和目录。

## 范围

仅调整内部模块、文件和入口别名的名称及路径，保留函数内容、条件编译、可见性、接口、数据库和执行行为。仍以 10 万用户、1 万节点为前提。本次没有数据迁移或部署。

## 已实施

1. 八个管理请求入口模块由 `v2` 改为 `handlers`：授权组、凭据、DNS、任务重试、托管 mTLS、节点、用户和订阅模板存储。保留原有领域 mutations、preparation、reads 等模块及其职责。
2. `api/event_stream_v2/` → `api/event_stream/`；`api/backend_cache_v2_tests.rs` → `api/management_contract_tests.rs`。测试函数名称保留，模块路径随名称调整。
3. 仅将用于区分 HTTP 入口的 `create_v2`、`patch_v2`、`delete_v2` 等内部别名改为对应的 `_handler` 名称，同步路由引用。普通协议相关测试名称、URL、事件游标、SSH 协议和数据库/Redis 版本命名空间保留。
4. 将仅包含一个 `mod.rs` 的授权组 handlers 和 credentials/identities 目录收为同名 `.rs` 文件。SQL 和 Lua 分别归入 `cache_domains/cache_batch_version.sql`、`quota_rank/quota_rank.lua`，资源字节保持一致。
5. 同步 Lua 专项验证脚本、数据库表用途文档及架构约定。最终 `src` 根目录只保留 `main.rs`、`state.rs`、`shutdown_tests.rs` 三个文件，其余模块及资源按所属目录归类。

## 验证

- 对全部服务端源代码展开模块，将资源路径及上述明确的标识符重命名统一；忽略 rustfmt 调整的连续模块声明顺序和可选尾逗号后，整理前后语法逐字一致。业务代码、协议字符串和测试函数内容保持一致。
- 两个移动资源的 SHA256 在移动前后完全一致。
- `src` 下没有以 v2 命名的文件或目录，代码中没有旧 v2 模块引用或 HTTP 入口别名。
- `cargo check --workspace --all-targets`、`cargo fmt --all -- --check` 和 `git diff --check`：通过。
- 测试目标编译通过；按模块重命名映射比较，原 1064 个测试与最终清单一一对应，没有新增或删除测试。
- 配置、安全、运行配置、SSH 解析及 ACME 单元用例：30 通过，0 失败。未运行完整数据库、多实例或部署回归。
- Lua 专项脚本的 Python 语法和资源路径检查通过；数据库表用途文档的全部本地链接有效。

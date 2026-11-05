# HysteriaX

## 项目结构

```text
crates/
  hysteriax-server/
    src/                       Rust 管理服务及所属模块测试
    migrations/                版本化数据库 SQL
  hysteriax-template-validator/ 固定 schema 验证器
apps/macos/
  Sources/HysteriaX/            macOS 应用源码与运行时资源
  Tests/                       客户端独立回归源码与本地化检查
    runners/                   客户端测试的编译与运行入口
  UITests/                     macOS UI 用例与 UI 夹具源码
components/hysteriax-acme/      节点独立 ACME 组件
scripts/
  build/                       构建、启动应用与打包发布
  generate/                    代码、元数据和许可生成工具
  ops/                         初始化、服务启动、备份恢复等运维工具
  migrations/                  一次性迁移工具
tests/
  unit/                        操作工具及测试辅助代码的 Python 单元测试
  integration/                 HTTP、数据库、节点、部署及升级验证
  contracts/                   OpenAPI、schema、配置与文档契约验证
  performance/                 容量、并发、锁争用与查询计划验证
  support/                     共享测试客户端、隔离环境与故障注入代码
  fixtures/                    共享证书、配置、固定语料和预期结果
    runtime/                   Docker、网络隔离和测试 CA 夹具
  requirements.txt             Python 测试依赖
openapi/                       管理 API 契约源文件
deploy/                        部署配置、节点脚本及许可资源
production/                    本地生产维护、发布与诊断记录
docs/                          当前开发、业务及运维指南
  plans/                       按编号保存的实施记录
.github/workflows/             CI 与发布工作流
```

- Rust 工作区与依赖声明位于根目录 `Cargo.toml`；容器构建与编排入口为 `Dockerfile`、`compose.yaml` 和 `compose.test.yaml`。
- 文件按职责归属：实际操作工具放在 `scripts/`，仓库级验证程序放在 `tests/`，客户端专属测试放在 `apps/macos/Tests/`，Rust 模块测试靠近所属实现。可执行脚本也遵循这套划分。
- 服务端领域组件和多实现分组使用目录；内部叶子模块使用具名文件，避免为一份实现及配套测试增加目录层级。API 边界可建立一级职责分组，测试可用明确的 `#[path]` 保持原模块关系。
- 移动入口时同步更新调用路径、根目录定位、资源引用、执行权限、CI 和当前文档；历史记录保留当时的路径与结果。

完整规则见 [目录与测试归属](docs/code-layout.md)，具体入口见 [测试索引](tests/README.md) 和 [操作脚本索引](scripts/README.md)。

## 维护准则

- 项目仍处于测试期，任何改动都不必兼容旧代码，可以对已有代码和数据库进行破坏性变更，同时要清理过时测试，但是要单独提供测试环境的一次性数据迁移工具以供部署使用。
- 做任何改动的前提假设：10w用户 和 1w 节点
- 生成的计划实施之后，按编号保存到 `docs/plans`
- 部署服务时，本地生成容器镜像推送到服务器
- 根目录 `README.md` 只能编写项目介绍和面向使用者的说明，包括项目定位、功能、支持平台、安装部署、连接客户端、日常操作、备份升级、故障处理入口及相关使用指南导航。

## 生产服务器部署与维护

参考 `production` 文件夹。

# macOS 客户端回归源码

本目录中的 Swift 文件是独立回归程序，由 [runners/](runners/) 下的入口选择应用源码、调用 `swiftc` 编译并运行。文件中的 `@main` 属于各自的测试程序；应用构建目标仍使用 `Sources/HysteriaX`。

`verify-localization-resources.py` 检查应用本地化键和占位符，由本地化验证入口调用。现有 UI 用例继续放在相邻的 `UITests/`，跨组件共享数据继续使用仓库根目录的 `tests/fixtures/`。

新增客户端验证源码放在本目录，其编译与运行入口放在 `runners/`；跨组件验证及共享辅助代码按 [目录与测试归属](../../../docs/code-layout.md) 放入仓库级测试目录。

从仓库根目录执行，例如：

```sh
bash apps/macos/Tests/runners/verify-localization.sh
bash apps/macos/Tests/runners/verify-node-task-feedback.sh
bash apps/macos/Tests/runners/verify-subscription-client.sh
```

更多仓库级入口及隔离服务要求见 [测试索引](../../../tests/README.md) 和 [部署与回归验证](../../../docs/deployment-testing.md)。构建及运维工具见 [操作脚本索引](../../../scripts/README.md)。

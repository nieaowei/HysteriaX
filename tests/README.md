# 仓库级测试索引

本目录统一保存仓库级验证程序、单元测试、性能测试、环境辅助代码和共享夹具。可执行的 Python、Shell 或 Ruby 验证程序也按测试职责归入这里。

与操作工具、组件专属测试的边界见 [目录与测试归属](../docs/code-layout.md)。新增验证程序按下表归组，正式验收的环境和门槛以各入口说明为准。

| 目录 | 内容 | 常用入口 |
| --- | --- | --- |
| [unit/](unit/) | 运维脚本、测试辅助代码和文档提取工具的 Python 单元测试 | `unittest discover` |
| [integration/](integration/) | HTTP、数据库、SSH、DNS、部署、升级和多实例功能验证 | `verify-deployment-matrix.py`、`verify-multi-instance-billing.py`、`verify-execution-finalization.py` |
| [integration/traffic-clear/](integration/traffic-clear/) | 独立 Go 模块，验证固定 Hysteria 的流量清零、在线状态和继续计费行为 | `go test` |
| [contracts/](contracts/) | OpenAPI、配置内核、固定 schema 和模板文档契约验证 | `verify-management-contract.rb`、`verify-subscription-validator-compatibility.py` |
| [performance/](performance/) | 容量、并发、锁争用、查询计划及大规模清理检查 | `benchmark-multi-instance.py`、`verify-lock-contention.py`、`verify-execution-finalization.py`、`verify-query-plans.sh` |
| [support/](support/) | 共享测试客户端、隔离数据库、故障注入和确定性夹具生成 | 由验证程序导入；`overview-fixtures.py` 也可直接运行 |
| [fixtures/](fixtures/) | 跨组件共享证书、配置、schema 评估语料与固定结果 | 保留固定输入和结果 |
| [fixtures/runtime/](fixtures/runtime/) | 测试节点镜像、网络隔离脚本和测试 CA 服务 | `systemd-node.Dockerfile`、`egress-guard.Dockerfile` |

Python 依赖见 [requirements.txt](requirements.txt)。从仓库根目录执行，例如：

```sh
python3 -m pip install -r tests/requirements.txt
python3 -m unittest discover -s tests/unit -p 'test_*.py'
ruby tests/contracts/verify-management-contract.rb
go -C tests/integration/traffic-clear test -count=1 ./...
bash apps/macos/Tests/runners/verify-localization.sh
```

组件专属测试靠近组件：独立 macOS 回归源码与运行入口位于 [apps/macos/Tests/](../apps/macos/Tests/README.md)，UI 用例保留在 `apps/macos/UITests/`，Rust 测试继续位于所属 Rust 模块中。

性能与功能验证分别调用。正式锁争用验收、快照容量和查询计划有独立规模、时长和环境要求，冒烟模式不能替代正式结果。环境要求及完整矩阵见 [部署与回归验证](../docs/deployment-testing.md)。构建、生成、运维和实际迁移工具见 [操作脚本索引](../scripts/README.md)。

# 脚本单元测试

本目录验证备份恢复、测试网络隔离、管理请求辅助代码及模板文档提取工具。测试继续使用原有的 mocks、临时文件和断言。

实际操作工具位于仓库根目录的 `scripts/`。测试归属和其他验证类型见 [目录与测试归属](../../docs/code-layout.md) 与 [仓库级测试索引](../README.md)。

从仓库根目录执行：

```sh
python3 -m unittest discover -s tests/unit -p 'test_*.py'
```

`test_template_documentation_audit.py` 已采用相同的发现命名规则，可与其他脚本单元测试一起运行。真实服务和故障注入验证入口位于相邻的 `integration/`，共享辅助代码位于 `support/`。

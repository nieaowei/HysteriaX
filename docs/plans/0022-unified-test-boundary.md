# 0022 统一仓库级测试与操作脚本边界

实施日期：2026-11-05（Asia/Shanghai）。按用户批准的方案，仓库级测试集中到 tests，组件测试靠近组件，scripts 保留操作工具。

## 范围

只调整文件归属、引用和仓库根定位，保留函数、断言、CLI 参数、数据隔离及正式性能验收门槛。仍以 10 万用户、1 万节点为规模前提，没有迁移数据库或部署服务。

## 已实施

1. 整体迁移 86 个文件：Python 单元测试及索引 5、集成验证 25（含原一次性迁移的配套验证）、契约验证 8、性能验证 5、共享辅助模块 13、运行时夹具 4、统一依赖文件 1、客户端运行入口 25。
2. scripts 最终只保留 build、generate、ops、migrations 四类工具及索引。实际迁移工具继续位于 scripts/migrations；verify-lock-migration.py 归入 tests/integration。
3. tests 统一包含 unit、integration、contracts、performance、support、fixtures、requirements.txt 和索引。原有独立 Go 模块、证书、配置和固定 schema 评估语料保持原位置与内容；Docker/CA 夹具归入 fixtures/runtime。
4. macOS 独立 Swift 回归源码保持在 apps/macos/Tests，25 个客户端运行入口归入该目录的 runners；现有 UITests 保持原位置。应用源码和构建目标未改动。
5. 同步更新 26 个原有引用文件，涵盖 CI、Docker COPY、发布脚本、测试加载、当前文档和测试忽略说明。修正 Python/Ruby/Shell 根定位与共享辅助模块导入路径，保留文件执行权限。
6. 重写操作脚本与仓库级测试索引，更新客户端索引和架构约定。历史审计、历史实施记录及私有生产归档保持原文，不将过去执行过的命令改写成新路径。

## 验证

- 57 个 Python 文件统一路径和根定位后，语法树与迁移前一致；26 个单元测试的完整标识逐项一致，迁移前后均通过。
- 新路径下 Go 流量清零测试以 -count=1 通过，管理协议契约检查和 Swift API 生成器 --check 通过。
- 24 个迁移 Shell 文件、1 个迁移 Ruby 文件语法检查通过；实际执行 20 个 Shell 根定位及求值 71 个 Python 根定位表达式，全部正确。
- 五个客户端入口通过：本地化、管理显示文本、节点任务反馈、ACME DNS 和订阅；订阅入口包含 URL、模板及字段布局三份独立 Swift 回归程序。
- 当前源码和文档没有旧目录引用，CI YAML 可解析，索引本地链接有效；文件权限保持。
- cargo fmt --all -- --check 与 git diff --check 通过。
- 未执行完整数据库、Docker、真实 SSH/DNS、UI 和正式性能测试，没有进行生产发布。

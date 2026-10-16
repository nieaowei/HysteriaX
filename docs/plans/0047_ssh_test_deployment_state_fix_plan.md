# 0047 SSH 测试保留部署状态修复与上线

日期：2026-10-16（Asia/Shanghai）。状态：已修复、部署并完成线上复测。上线时间：2026-10-16 15:59:26。

## 问题

SSH 测试成功后除卸载恢复外固定返回 `ready`，导致已部署节点显示为“待部署”。确认指纹时的等待状态也会覆盖原有部署、回滚或失败提示。线上发现“美国旧金山”和“韩国春川”受影响，部署版本记录仍分别为 3 和 9；最近的正常 SSH 测试使节点显示为 `ready`，远端部署并未被卸载。

## 实施步骤

1. **保留独立状态（已完成）**：SSH 测试保留已有 deployed、rolled_back、sync_failed、rollback_failed、drift、syncing 状态。新节点连接检查通过后仍可进入 ready；卸载失败诊断继续保留 delete_failed。
2. **跨指纹确认恢复（已完成）**：在等待结果中保存 `ssh_restore_state`，确认后带入原任务 payload，避免重新连接或服务重启丢失原状态。确认后认证失败也保留原有部署及失败提示。
3. **兼容已有错误状态（已完成）**：已记录 deployed_revision 的节点处于 ready 或连接告警时，依据成功部署／回滚记录恢复部署状态。保持 desired_revision、deployed_revision、配置和 SSH 绑定不变；保留已有版本校验及并发删除保护。
4. **本地构建与上线（已完成）**：构建 amd64 镜像 `nieaowei/hysteriax-server:ssh-state-fix-20261016-0047`，隔离数据库验证启动与现有 0030 schema；以 SCP 推送并 docker load，核对镜像层、运行配置及二进制。备份后只替换 API，PostgreSQL、Redis、Caddy 容器实例保持不变。本次没有新数据库迁移。
5. **恢复与线上验证（已完成）**：通过正常 SSH 测试恢复两台节点，并各重复一次，4 个任务均成功且状态持续为 deployed。逐次核对配置、SSH 绑定、当前版本及部署版本摘要一致。未执行节点部署、同步、回滚或卸载，也未直接修改生产状态表。

## 验证

- `cargo test --workspace`：234 项通过，1 项原有 benchmark 跳过。
- 新增网络与数据库集成回归：6 种部署状态分别经过指纹确认和直接测试；旧 ready／连接告警恢复；确认后认证失败保留既有部署提示。原有指纹恢复、卸载失败、版本冲突和并发删除测试均通过。
- 本地 amd64 镜像启动、0030 schema、等待任务、确认接口鉴权及概览检查通过。
- 线上 API healthy，RestartCount=0，启动后 ERROR 日志行数为 0。5 个节点采样全部推进，6 份已有 sing-box 订阅检查通过，管理统计及凭据详情／引用接口正常。
- “美国旧金山”保持当前版本 5、部署版本 3；“韩国春川”保持当前版本 11、部署版本 9。两台各连续两次 SSH 测试均成功并保持已部署。
- 本次仅后端修复，客户端无新增改动或安装包发布。

## 交付及回退

服务端二进制 SHA-256：`6cf0297a5b94624fe42d5066baa385dbddcb1932c93a3615d7b71bd1859883f5`。

备份目录：`/home/nieaowei/HysteriaX/backups/ssh-state-fix-20261016-0047`。数据库 dump 为 44611639 字节，SHA-256 为 `ea544a69523357d25c09bfa4ec4d6f8d82c20a9cee6513e276a0df052bea4458`；环境副本权限 0600，备份目录 0700。完整源码、摘要、原镜像和回退入口已保留。

旧镜像：`nieaowei/hysteriax-server:ssh-flow-20261016-0045`。回退入口为备份目录中的 `rollback.sh`，恢复原环境及旧镜像，保持 schema 0030，不还原业务数据库或覆盖后续采样。未知 schema 会拒绝自动回退。本次没有触发或在线演练回退。

详细结果及 4 个线上 SSH 测试任务 ID 见 [验证记录](0047_ssh_test_deployment_state_fix_verification.json)。

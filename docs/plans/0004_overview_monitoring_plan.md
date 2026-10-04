# 0004 — 完整监控概览

## 已确定的目标

概览以发现问题并处理为中心，覆盖节点部署状态、持续代理探测、套餐风险、在线用户与连接、今日／7 天／30 天历史、用户状态、任务与管理服务器资源。管理服务器发起公网代理探测；默认验证入口与转发，配置了 `proxyProbeUrl` 的节点追加外部目标验证。新增监控提醒仅在应用内显示，保留既有套餐系统通知。

## 实现

- 管理员只读接口 `GET /api/v1/overview` 聚合全量任务计数、异常、节点当前监控与额度排行；`GET /api/v1/overview/history` 接受 `range=today|7d|30d`、IANA `timezone`、可选 `node_id` 和 `source=users|network`。
- 版本能力标记 `overview_monitoring` 控制新客户端功能。旧服务显示基础概览；高级接口失败只保留带时间的旧快照，不让其他管理功能失去连接状态。
- 用户代理流量复用 `traffic_records`；节点网卡流量复用 `node_network_samples`。上下行从节点视角标记为发送、接收，独立于套餐计量方向和周期。全部已部署节点采集网卡流量，即使没有设置额度。
- 迁移 0004 新增在线采样、代理探测结果、探测健康状态、短期凭据及租约表；为流量记录新增 `baseline_only` 标记，回填历史首个基线，不改变计费增量或已用额度。
- 在线统计通过现有 SSH 连接读取 `/online`，过滤非已分配用户，排除部署与监控探测身份。当前用户跨节点去重，连接按节点累加；历史先按统一采集周期去重，再聚合平均值与峰值。部分覆盖和陈旧数据显式标记。
- 默认每节点 60 秒探测一次，单次总超时 15 秒，最多并发 4 个节点；55 秒数据库租约阻止重复工作。探测客户端运行于管理服务器，凭据有效期 30 秒，与部署探测凭据分表。
- 使用已部署修订的配置和资源；新快照保存公网地址、TLS SNI 与验证设置。旧快照回退到已保存的连接设置。复用 TLS／mTLS／ECH／Realm 配置解析；缺少 mTLS 客户端证书显示未配置。
- 连续 3 次失败告警，连续 2 次成功恢复；修订变化或采样间隔超过 180 秒重置连续状态。入口结果与外部目标结果独立显示。请求耗时不是网络 RTT；历史 P50／P95 仅使用有效成功样本。客户端退出仅返回固定分类原因，不暴露原始日志中的凭据。
- 今日按小时、7 天和 30 天按本地日聚合，数据库时间采用 UTC。夏令时重复小时独立计数。成功空流量响应返回测得的零；采集失败或缺失返回空值及覆盖／缺口标记，首次基线、采样缺口与计数器重置返回不完整标记，不改写计费规则。
- 新监控历史保留 30 天，每日分批清理；现有计费记录与配额基线不参与清理。
- macOS 概览包含 4／2 列摘要卡片、异常与额度排行、节点监控详情、Swift Charts 趋势、用户摘要及最近任务。异常可定位节点、用户、任务；概览不直接执行部署、撤权或额度重置。
- 概览摘要沿用 15 秒刷新，历史仅在可见时每 60 秒或条件改变时加载。历史缓存按服务、时区、范围、节点和来源隔离，最多保留 16 组；取消和服务切换丢弃旧响应。

## 运行与兼容

容器包含与节点一致的 Hysteria v2.12.3 客户端，并校验仓库已固定的发行文件 SHA-256。默认探测路径 `/usr/local/bin/hysteria`；直接运行 Rust 服务时用 `HYSTERIAX_PROBE_BINARY` 指向匹配版本的客户端。缺少客户端时显示未配置，管理服务继续运行。

节点公网地址必须从管理服务器可达。没有外部探测网址时仅表示公网入口和节点统计端点转发可用，不能解释为所有外部网站可达。已有 `proxyProbeUrl` 继续遵循项目 HTTP URL 校验规则。资源和临时客户端配置在探测结束、失败或取消时清理；日志与 API 不返回探测凭据和私钥。

生产部署遵循仓库约定：本地构建目标架构镜像，再推送服务器。此次真实链路验证使用临时 Linux/systemd 节点及独立数据库，不修改既有管理节点。

## 验证命令

```sh
TEST_DATABASE_URL=postgres://… cargo test -p hysteriax-server
ruby scripts/generate-swift-api-models.rb --check
TEST_DATABASE_URL=postgres://… python scripts/verify-migrations.py
scripts/verify-overview-client.sh
scripts/verify-overview-network.sh
python3 scripts/verify-overview-ui.py
# Xcode UI runner 不可用时，可构建独立窗口用原生控制验证：
python3 scripts/verify-overview-ui.py --manual
TEST_DATABASE_URL=postgres://… python scripts/verify-overview-live.py
./script/build_and_run.sh --verify
docker build --platform linux/arm64 -t hysteriax-server:overview-20261003 .
```

Python 接口验证需要 `scripts/requirements-test.txt`；真实节点验证需要 Docker 和 `hysteriax-package-test:local` systemd 测试镜像。实例锁测试与实时 API 验证应使用不同数据库，单独 schema 不隔离数据库级 advisory lock。

## 验收覆盖

数据库测试验证在线去重、部分失败、未知与零值、全量任务计数、日期与夏令时边界、探测失败及恢复阈值、短期凭据过期、租约互斥、mTLS 缺少证书、临时文件清理及监控历史清理不影响计费。客户端测试验证契约解码、缓存隔离、深浅色宽窄布局、新旧服务兼容和高级接口独立失败。

真实 Hysteria 节点验证覆盖管理服务器经公网入口请求外部目标、外部故障与恢复、在线用户身份排除、两种流量来源、三个历史范围和临时凭据清理。UI 测试用隔离 fixture 与临时 bundle，覆盖异常定位、节点详情、通知面板和 Escape 关闭；执行状态见同目录验证记录。

## 本地实时验证的环境差异

本机 OrbStack 的 UDP 发布转发在监听器停止数分钟后未随进程启动恢复。对同一测试节点，容器 IP 上的双向 UDP 回显正常，而发布端口回显超时；重启该临时容器重新建立发布路径后，回显恢复。因此实时故障恢复验证必须同时恢复实际入口转发，不能仅依据 systemd 的 active 状态断言公网链路已恢复。验证脚本在恢复阶段重启一次临时容器；生产探测逻辑保留真实入口失败告警。

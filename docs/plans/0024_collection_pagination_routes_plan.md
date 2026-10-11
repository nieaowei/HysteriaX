# 0024 分页功能合并到原有集合路径

日期：2026-10-11

## 目标

移除节点、用户、DNS 记录和凭据的 `/page` 路由及全量响应模式，原有集合接口统一返回分页结果。

## 实施内容

1. 原有 `GET /api/v1/nodes`、`GET /api/v1/users`、`GET /api/v1/dns/records`、`GET /api/v1/credentials` 始终返回 `{ items, total, page, page_size }`。
   - 不传分页参数时返回第 1 页，每页 50 条，最大每页 200 条。
   - 搜索、筛选和排序即使没有显式分页参数也生效；保留已有参数校验、稳定排序、越界页码处理及摘要字段。
   - 分页逻辑直接合并到原有 `list` / `list_records`，删除 `list_query` 分发层、独立分页函数及 `list_all`。参数类型统一命名为 `*ListQuery`。
2. OpenAPI 将分页参数和单一分页响应迁移到原路径，删除 `/page` 路径及数组/分页两种响应的兼容扩展。恢复普通 Swift 生成流程，客户端统一使用 `listNodes`、`listUsers`、`listCredentials`、`listDNSRecords`。
3. 客户端刷新缓存和选择器需要完整目录时，使用 `APIClient.collectPages` 逐页读取，每次最多 200 条。校验响应页码、条数和分页进度，支持取消及失败传播，避免服务端页码回退造成重复装配。页面列表继续按当前页读取。
4. 服务端概览逐页读取节点与用户，删除全量查询辅助函数，确保统计覆盖第 2 页以后的数据。DNS 实时验证脚本和 macOS 实时 UI 脚本也改为遍历分页。
5. 更新 Rust 分页测试、真实 HTTP 路由测试、Swift 网络与 DNS 契约测试、SSH 指纹回归和 DNS/授权组 UI 夹具。
   - 验证默认分页对象、50 条默认上限、仅页码/仅条数、无分页参数的搜索/筛选、非法参数、鉴权及旧 `/page` URL 返回 404。
   - 节点/用户/DNS/凭据目录超过 200 条的分页回归保持通过；235 个节点的概览聚合验证通过。
   - 客户端刷新实际遍历 235 个节点，验证完整性和标识不重复。

## 验证结果

- `cargo test --workspace -q`：172 项测试通过。
- `cargo check --workspace --all-targets`：通过。
- `cargo clippy --workspace --all-targets -- -D warnings -A clippy::duplicated_attributes -A clippy::ptr_arg -A clippy::too_many_arguments`：通过，沿用此前记录的既有问题排除项。
- `scripts/verify-overview-network.sh`：通过，包括跨页刷新、四类分页路径、中文/特殊字符查询、分页解码及缓存/取消/服务切换回归。
- `scripts/verify-dns-client.sh`：通过，默认列表调用使用服务端分页默认值。
- `scripts/verify-authorization-groups-client.sh`：通过，包括 5,000 位成员、20,000 条连接、跨页选择和独立页状态。
- `scripts/verify-ssh-fingerprint-client.sh`：通过。
- 两个 UI 夹具的 Python 语法检查及提取后 Swift 6 类型检查通过；未执行完整 UI 操作验收。
- 实时验证脚本的 Python 语法检查及模拟分页验证通过：235 项跨两页、区域筛选保留和失败传播；未对公网服务执行实时验收。
- `ruby scripts/generate-swift-api-models.rb --check`、`git diff --check`：通过。

## 发布状态（已完成）

- 2026-10-11 19:16（Asia/Shanghai）部署至 `https://hysteriax.cc`，API 容器健康，公网服务及数据库就绪。
- 镜像 `nieaowei/hysteriax-server:collection-pagination-20261011-v1` 在本地构建为 `linux/amd64`，导出归档后上传服务器并导入 Docker。
- 归档 SHA-256、文件系统层、完整运行配置、架构和二进制 SHA-256 均已核对。服务器 Docker 导入后的镜像 ID 与本地表示不同，以服务器实际 ID 确认运行容器；两端层和运行配置完全一致。
- 配置及一致性数据库备份目录：`/home/nieaowei/HysteriaX/backups/collection-pagination-20261011-v1`。备份时停止 API，PostgreSQL 自定义格式备份已通过 `pg_restore --list` 读取验证。
- PostgreSQL 和 Caddy 容器标识保持一致。数据库迁移仍为 15，全部迁移成功。
- 四类原有列表接口默认返回分页对象；每页 2 条遍历、无重复/遗漏、升降序、越界页码、空搜索、无显式分页参数的搜索/筛选、非法参数、鉴权及旧 `/page` URL 返回 404 全部通过。
- 部署前后的 5 个节点、7 个用户、22 条 DNS 记录和 50 项凭据的标识及适用修订号/版本、名称、启用/归档和绑定字段保持一致。
- 节点/用户/记录及托管凭据详情核对、DNS 区域筛选、凭据分类、任务、审计、概览和公网 OpenAPI 契约验证通过。
- 使用 `./script/build_and_run.sh --verify` 构建并启动配套的本地 macOS 客户端；在服务健康后重新启动验证成功。旧客户端需要更新以使用新的分页响应。
- 镜像 ID、校验值、验证时间及容器标识见 `0024_collection_pagination_deployment_verification.json`。

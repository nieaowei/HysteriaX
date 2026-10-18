# 节点 ACME 维护

`hysteriax-acme` 固定使用 CertMagic v0.25.4，DNS 服务商及版本与 Hy2 v2.12.3 一致。组件源码、锁文件和依赖许可在 `components/hysteriax-acme/`。本地运行 `scripts/build-node-acme.sh` 生成 Linux amd64 / arm64 二进制、许可包和 SHA256SUMS；管理服务镜像在本地构建时也会包含两种架构的产物，通过现有 SFTP 下发，节点无需安装 Go。部署管理服务时仍须本地构建镜像后推送服务器。

## 文件与权限

- `/etc/hysteriax/acme.json`：组件配置及 DNS 秘密，仅证书账号和 root 可读。
- `/var/lib/hysteriax-acme/<策略标识>/`：版本化状态、ZeroSSL EAB、失败次数、下次重试及发布恢复日志，仅证书账号可读。
- `/var/lib/hysteriax-acme/node.lock`：组件与部署事务共享的本地写锁。
- `/var/lib/hysteriax/certificates/<策略标识>/versions/`：不可变证书版本；`current` 链接原子切换。Hy2 仅可读取公开链和对应服务端私钥，不能读取 ACME 账户或 DNS 秘密。
- `acme.dir`：保留 CertMagic 节点目录含义，空值默认 `/var/lib/hysteriax/acme`，相对值基于 `/var/lib/hysteriax` 解析。迁移拒绝符号链接、外部属主和没有项目所有权证明的已有目录。不要将它指向网站、系统目录或其他应用的数据。
- `/opt/hysteriax/acme-previous/`：上一套组件配置、二进制及 systemd 单元，供失败恢复使用。切回手动 TLS 时保留恢复材料。

组件使用独立账号和 `CAP_NET_BIND_SERVICE`。首次 ensure 使用相同受限身份及能力，所有秘密通过文件传递。HTTP-01 / TLS-ALPN-01 备用端口仍要求公网 80 / 443 正确转发；组件不会更改防火墙或停止已有网站。端口占用与权限错误应先修复后同步。

## 命令和周期

管理员可在节点执行（命令输出不含私钥和 DNS 凭据）：

```sh
sudo /opt/hysteriax/hysteriax-acme check /etc/hysteriax/acme.json
sudo /opt/hysteriax/hysteriax-acme status /etc/hysteriax/acme.json
sudo systemctl status hysteriax-acme.service
```

`ensure` 确保存在有效、完整覆盖域名的证书；`run` 启动时及每小时检查，在证书总有效期的最后三分之一开始续期。每次签发使用新的 ECDSA P-256 密钥。失败从一分钟指数退避到六小时并加抖动；CA 请求要求更长的等待时采用更晚的时间，状态跨重启保留。ZeroSSL 邮箱获取的 EAB 仅保存在私有状态目录中。未知 DNS 参数仍保留但不启用新的行为。

管理服务的 SSH 采集将状态绑定实际部署修订、配置摘要和策略；旧采样不会覆盖新部署。证书状态采集失败不阻止流量统计。macOS 可显示签发、续期、失败、到期、停止和陈旧状态；启用既有节点通知后，提供续期失败及 30 / 14 / 7 天到期提醒，并跨应用重启去重。

回滚恢复目标策略并重新检查证书有效性，不强制切回过期叶证书。发生结果不明的 SSH 中断时，先检查部署任务与节点状态，再使用显式恢复同步。组件账户、状态和证书应独立备份，详见 `backup-restore.md`。外部自定义 `acme.dir` 不在自动卸载清理范围，需管理员确认独立备份及目录所有权后处理。

## 可重复验证

```sh
cd components/hysteriax-acme
go test -race ./...
go vet ./...
# 可选隔离测试 CA，不能替换为生产 CA：
GOBIN=/tmp/hysteriax-acme-tools go install github.com/letsencrypt/pebble/v2/cmd/pebble@v2.10.1
PEBBLE_BIN=/tmp/hysteriax-acme-tools/pebble go test -race -run TestPebbleChallenges -v ./...
```

回到仓库根目录，构建组件并运行 systemd 验收：

```sh
scripts/build-node-acme.sh
docker build -f scripts/fixtures/systemd-node.Dockerfile -t hysteriax-acme-test-node:0003 .
scripts/verify-node-acme-systemd.py
# 使用与测试容器同架构的 Linux Pebble 二进制，增加首次签发及真实超时恢复：
PEBBLE_NODE_BIN=/path/to/linux/pebble scripts/verify-node-acme-systemd.py
```

Pebble 测试运行本地 DNS stub 和测试 provider，实际验证 HTTP-01、TLS-ALPN-01、DNS-01、备用端口及多域名签发；八家真实服务商仅使用现有 fixtures 验证字段映射，不访问真实账号。systemd 验收使用一次性容器和本地测试 CA，测试安装、兼容 SAN 复用、账号权限、组件重启、原版 Hy2 的 SAN+mTLS+ECH 转发、失败恢复及手动 TLS 切换。

启用 `PEBBLE_NODE_BIN` 的验收仅在一次性容器内把 ACME 目录域名映射到本地 HTTPS 测试代理，并校验账户/订单地址仅使用已解析到回环地址的同一本地域名映射；不会访问生产 CA。该流程实际等待签发取消期限后检查上一套策略和服务恢复。

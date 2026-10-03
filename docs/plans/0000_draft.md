# HysteriaX

基于Rust构建的hysteria2(hy2)服务器节点管理项目，提供macOS原生客户端。

## 预期功能

### 节点管理

1. 可视化配置服务器节点：服务器SSH配置和hy2服务器配置
2. 自动化部署：提供一键部署功能，使用上面SSH配置访问服务器，使用上面hy2服务器配置在服务器上安装。
3. 配置自动化同步：当hy2服务器配置改变时同步到已经部署的服务器上。


### 用户管理

1. 用户新增、删除、修改
2. 为用户生成订阅密钥，分配节点
3. 为用户生成带有分配节点的clash配置文件


## 引用

[hy2服务器配置](https://v2.hysteria.network/zh/docs/advanced/Full-Server-Config/)
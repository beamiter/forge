# Tailscale 远程连接配置

客户端 ←→ 远程 Linux 主机（示例局域网地址 `192.0.2.10`）。
`192.0.2.10` 属于 RFC 5737 文档专用网段，不是真实设备地址。Tailscale 可组成
mesh VPN 连接两端，穿透 NAT/CGNAT，无需公网 IP 或调整路由器。

以下以 Ubuntu/Debian 为主，并附 NixOS 写法。

## 远程 Linux 主机

```sh
# 安装（官方一键脚本，Ubuntu/Debian/多数发行版通用）
curl -fsSL https://tailscale.com/install.sh | sh

# 启动并登录（会打印一个 URL，浏览器打开授权）
sudo tailscale up

# 可选：启用 Tailscale 内置 SSH，免维护 sshd 密钥
sudo tailscale up --ssh

# 查看分到的 Tailscale IP 和设备名
tailscale ip -4
tailscale status
```

> 对需要长期无人值守的远程主机，可以在 Tailscale 管理后台评估
> **Disable key expiry**。这会改变身份证书的轮换策略，应与设备风险和团队政策一起评估。

## 客户端

```sh
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up            # 同一账号登录，授权后两台互通
tailscale status             # 应能看到远程主机
```

## 验证连通

```sh
# 在客户端 ping 远程主机的 Tailscale IP
tailscale ping <remote-tailscale-ip>

# 直接 SSH（使用 Tailscale IP，或启用 MagicDNS 后使用设备名）
ssh <user>@<remote-tailscale-ip>
ssh <user>@<remote-hostname>
```

## 要点

- **同一账号/tailnet**：两端 `tailscale up` 必须登录同一账号，否则看不到彼此。
- **MagicDNS**：admin 后台（login.tailscale.com → DNS）打开后，可用机器名代替 IP。
- **Key 过期**：无人值守主机需要明确的证书轮换和丢失设备撤销策略。
- **`--ssh` 选项**：启用后 Tailscale 基于身份处理 SSH 认证，免维护 `authorized_keys`；
  想继续用现有 sshd + 密钥则不加。

## NixOS 版本

```nix
# configuration.nix
services.tailscale.enable = true;
```

```sh
sudo nixos-rebuild switch
sudo tailscale up
```

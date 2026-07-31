<h1 align="center">Sumire</h1>

<h3 align="center">面向 ZJU 网络访问的轻量客户端</h3>

<p align="center">
  <a href="https://github.com/Eclipsky1337/sumire/actions/workflows/ci.yml"><img src="https://github.com/Eclipsky1337/sumire/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/Eclipsky1337/sumire/releases"><img src="https://img.shields.io/github/v/release/Eclipsky1337/sumire?include_prereleases" alt="Release"></a>
</p>

Sumire 是 [zju-portal-core](https://github.com/Eclipsky1337/zju-portal-core) 的轻量 WebUI，提供会话控制、认证、配置管理和运行状态展示。

## Features

- REST Token 连接与 Control Protocol v2 检查；
- Session 启停、状态和路由模式切换；
- Windows HTTP/HTTPS 与 macOS HTTP/HTTPS/SOCKS 系统代理开关；
- 托管模式 TUN 快捷开关和启动权限检查；
- configured/active 配置快照、待应用字段高亮与显式应用；
- Session 配置显式应用，以及托管模式 Core 配置落盘和重启；
- SSE 认证 challenge（密码、短信、TOTP、CAS/OAuth、图形验证码点击和认证方式选择）；
- 流量、服务、逻辑连接和实时事件展示。
- 托管 Core 的 stdout/stderr 实时日志查看。

## Demo

![dashboard](./dashboard.png)

## Quick Start

### 托管模式（推荐）

Release 压缩包中将 WebUI 和 Core 放在同一目录：

```text
sumire/
├── sumire
└── zju-portal-core
```

MacOS需要先信任应用

```bash
xattr -d com.apple.quarantine sumire
xattr -d com.apple.quarantine zju-portal-core
```

直接运行 WebUI 即可，初次使用会自动生成一份config.yaml：

```bash
./sumire
```

在浏览器中访问显示的地址。在配置页面填写用户名和密码并保存后：

- 页面提示“重启 Session”时，点击“重启 Session 应用”；
- 页面提示“重启 Core”时，点击“重启 Core 应用”；
- 配置生效后，在概览页面启动会话。

> Note: 如果你不确定某项配置的含义，请不要随意修改配置文件

WebUI 会自动发现同目录的 `zju-portal-core`（Windows 为 `zju-portal-core.exe`），Core 随 WebUI 自动启动并在异常退出后自动重启。托管数据默认保存在 WebUI 同目录的 `data` 文件夹。

每个 Sumire Release 会自动打包发布时最新的 `zju-portal-core` Release。

托管模式默认使用：

- `data/config.yaml`：Core 配置；
- `data/resume-state.json`：Resume State；
- `data/control.token`：REST Token；
- `127.0.0.1:9090`：Core REST 地址。

Windows 和 macOS 可在概览页面开启系统代理。Windows 使用当前 active 配置中的 HTTP 入站设置 HTTP/HTTPS 代理；macOS 还会同时使用 active SOCKS5 入站设置 SOCKS 代理。开启后每 5 秒检查一次系统设置，被其他软件修改时会自动重新应用。开启时会覆盖现有代理，关闭时直接清空，不恢复之前的代理设置。Linux 暂不提供此功能。

托管模式可在概览页面切换 TUN。单击开关后 Sumire 会写入 `data/config.yaml` 并自动重启 Core 应用配置。启用 TUN 时 Sumire 自身必须具有系统网络管理权限：macOS/Linux 使用 `sudo ./sumire` 启动，Windows 需要以管理员身份运行。配置已启用 TUN 但当前进程权限不足时，Sumire 会拒绝启动；普通权限运行时页面也不会允许开启 TUN。

Core 完整输出默认只显示在 WebUI 的 Core 日志页面。排查启动问题时，可以使用 `./sumire -core-log-console` 将 Core stdout/stderr 同步输出到终端。

托管模式允许监听非回环地址，例如：

```bash
./sumire -listen :9080
```

启动时会输出安全警告。任何能够访问该 WebUI 的客户端都可以取得托管 Core Token 并控制会话，因此只应在可信网络中使用。

### 外部模式

也可以连接手动启动的 Core。Core 使用默认地址时只需：

```bash
./sumire external
```

连接其他地址时可以直接把地址放在末尾，`http://` 可省略：

```bash
./sumire external 192.168.1.10:9090
```

需要让 WebUI 监听所有网络接口时：

```bash
./sumire external -listen :9080
```

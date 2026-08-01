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
- Windows 与 macOS 系统代理开关；
- 托管模式 TUN 快捷开关和启动权限检查；
- 配置快照、待应用字段高亮与显式应用；
- 图形化认证（密码、短信、TOTP、CAS/OAuth、图形验证码点击和认证方式选择）；
- 流量、服务、逻辑连接和实时事件展示和 core 实时日志查看。

## Demo

![dashboard](./dashboard.png)

## Quick Start

### 托管模式（推荐）

想从Release下载对应平台和架构的压缩包并解压，压缩包中结构如下：

```text
sumire/
├── sumire(.exe)
└── zju-portal-core(.exe)
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

终端会显示监听地址，在浏览器中访问显示的地址即可使用 WebUI

在`配置`页面修改启动配置，默认配置推荐只修改username和password，点击主页启动即可，初次启动会进入认证流程

> Note: 配置含义参照配置中注释，如果你不确定某项配置的含义，最好不要随意修改配置文件

默认使用：

- `data/config.yaml`：Core 配置；
- `data/resume-state.json`：Resume State；
- `data/control.token`：REST Token；
- `127.0.0.1:9090`：Core REST 地址。

允许监听非回环地址，例如：

```bash
./sumire -listen :9080
```

Core 完整输出默认只显示在 WebUI 的 Core 日志页面。排查启动问题时，可以使用 `./sumire -core-log-console` 将 Core stdout/stderr 同步输出到终端.

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
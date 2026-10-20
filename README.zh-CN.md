# THUnet

[English](README.md) | **简体中文**

一个跨平台的小托盘程序：登录清华校园网（`auth4.tsinghua.edu.cn`，SRun3K 门户），
并让连接一直保持在线。

只有一个可执行文件，没有运行时依赖，没有控制台窗口，可在 Windows、Linux 和 macOS
上运行，界面支持中文和英文。

> **第一次使用？** 直接看 [1. 快速开始](#1-快速开始) —— 只需要 TUNET 账号和密码。
> 协议细节在文档最后，日常使用完全不需要了解。

## 目录

**入门**

1. [快速开始](#1-快速开始)
2. [界面一览](#2-界面一览)
3. [日常操作](#3-日常操作)
4. [疑难排查](#4-疑难排查)
5. [命令行](#5-命令行)

**参考**

6. [数据存在哪里](#6-数据存在哪里)
7. [安装、卸载、快捷方式](#7-安装卸载快捷方式)
8. [用 winget 安装](#8-用-winget-安装)

**给好奇的人 / 开发者**

9. [网络登录到底怎么做的](#9-网络登录到底怎么做的)
10. [从源码构建](#10-从源码构建)
11. [项目结构](#11-项目结构)
12. [限制与说明](#12-限制与说明)
13. [许可证](#13-许可证)

---

## 1. 快速开始

1. **拿到可执行文件。** 推荐直接用安装包，或者自己构建（见
   [从源码构建](#10-从源码构建)）：

   | 系统 | 文件 | 安装方式 |
   | --- | --- | --- |
   | Windows | `thunet-<version>-x64.msi` | 双击、`msiexec /i thunet-<version>-x64.msi`，发布后也可以 `winget install --id THUnet.THUnet -e`（见 [用 winget 安装](#8-用-winget-安装)） |
   | Debian / Ubuntu | `thunet_<version>-1_amd64.deb` | `sudo apt install ./thunet_<version>-1_amd64.deb` |
   | 其他 Linux | `thunet-<version>-linux-x64.tar.gz` | 解压后运行 `./thunet`（可选 `./thunet shortcut` 生成桌面图标） |

2. **运行。** 从开始菜单 / 应用菜单打开 `THUnet`，或在终端里执行 `thunet`。
   会弹出一个小窗口，程序做的每件事都会写到窗口底部的日志条里。
3. **打开设置。** 点窗口右上角的按钮。
4. **填写账号密码。** 输入 TUNET 账号（例如 `1234567890`）和密码，勾选
   **记住密码**，再点 **应用**。Windows 上密码会交给凭据管理器，绝不会写进设置文件。
5. **连接。** 回到状态页点 **连接**（如果"连接时自动连接"已经打开，等几秒即可）。
   标题处的圆点变成绿色就表示在线。

## 2. 界面一览

窗口只有两页，用右上角的按钮切换。

![THUnet 状态页](docs/images/zh-status.png)

*状态页（账号、地址和用户名已打码）。*

### 状态页

| 元素 | 含义 |
| --- | --- |
| 彩色圆点 + 文字 | `在线`、`认证中` 或 `离线` |
| `账号` | 门户报告的本会话账号 |
| `IPv4` / `IPv6` | 校园网分配的地址，各带一个**复制**按钮 |
| `本次流量` | 本次会话的下载 / 上传字节数 |
| **连接** / **断开** / **刷新** | 手动操作，只启用当前有意义的那几个 |
| 日志条 | 最近发生的事件，最新的在最下面 |

日志长这样：

```
[2026-10-20 12:56:26] [INFO] connected
[2026-10-20 12:56:41] [DEBUG] portal: auth4.tsinghua.edu.cn
```

每个级别（`DEBUG`、`INFO`、`WARN`、`ERROR`、`FATAL`）都有自己的颜色。日志正文
始终是英文 —— 它是给开发者看的诊断信息，因此不跟随*语言*设置；窗口里的其他内容
都会跟随。日志条显示最新的若干行，可以滚动。

*设置 → 日志级别* 会隐藏低于所选级别的行（`DEBUG` 显示全部，然后是 `INFO`、
`WARN`、`ERROR`）。隐藏只影响显示：日志并没有被丢弃，把级别调低后还会回来。

在选中的日志文本上右键可以直接复制到剪贴板。没有选中任何内容时，右键会弹出一个小
菜单，包含 **复制本行** 和 **全部复制**（两者都只复制当前级别可见的内容）。

### 设置页

![THUnet 设置页](docs/images/zh-settings.png)

*设置页（账号和密码已打码）。*

| 设置项 | 作用 |
| --- | --- |
| 开机自启 | 为当前用户添加或删除自启项 |
| 启动时最小化到托盘 | 与上一项相互独立：可以自启但隐藏，也可以手动启动时打开窗口，反之亦然 |
| 启动时自动连接 | 一检测到可用网络就登录 |
| 创建桌面快捷方式 | 为当前运行的可执行文件在桌面生成一个启动器（等同于 `thunet shortcut`） |
| 账号 / 密码 / 记住密码 / 应用 / 还原 | 这是唯一一行不会边输入边保存的设置：内容与已保存值不同时会变成强调色并出现 *未保存* 标记。应用会把密码交给操作系统凭据存储（不会写进 JSON），并立刻交给正在运行的会话；还原会丢弃修改，恢复已保存的账号、密码和勾选状态（关闭*记住密码*时它是灰的，因为没有可恢复的内容） |
| 语言 | 跟随系统（默认）、中文、English |
| 日志级别 | `DEBUG` / `INFO` / `WARN` / `ERROR`：日志页显示的最低级别（默认 `INFO`，存在 `config.json` 里） |
| 版本 | 左边是版本号，右边是构建时间（本地时钟，精确到秒，并带 UTC 偏移） |

三个开关在切换后会用一个提示条确认当前状态（*开机自启：开*），因为页面上没有别的
东西会随之变化。

**连接**用的是后台工作线程持有的密码：启动时是凭据管理器里的值（必须保存过*记住
密码*），之后是最后一次应用的值 —— 在点 **应用** 之前，输入框里的文字不会生效，
*未保存* 标记提醒的正是这件事。取消*记住密码*并应用会删除已保存的条目，但刚刚输入
的密码在程序退出前仍然可用。完全没有任何密码时，**连接** 会直接报告这一点，而不是
向门户发送一次空登录。

没有保存密码并不等于离线：客户端无论有没有密码都会向门户查询会话状态，所以已经
存在的会话（今天建立的，或者本网络里另一台设备建立的）依然会显示为在线，并带有
账号、地址和流量。

构建标记左边是版本，右边是编译这个二进制时的本地时间及其 UTC 偏移
（`构建于 2026-10-20 19:23:23 +08:00`）。两者都是构建期嵌入的（`build.rs`），
所以标记永远与正在运行的可执行文件一致 —— 当你拿不准更新有没有生效时很有用。
时间是按查看者所在时区渲染的，所以同一个二进制在别的时区机器上读出来不一样；
偏移量说明了当前看到的是哪个时区。

**不在校园网内时，连接按钮是灰的。** 登录只有在校园门户后面才能成功，所以客户端
每隔几秒探测一次网络，在以下情况禁用该按钮（鼠标悬停会给出原因）：机器接在
`Tsinghua-Secure` 这类 802.1X 网络上、完全没有链路、处在别人的认证门户后面，
或者已经能直接上网而不需要门户。**刷新** 在任何情况下都可用，**断开** 只看会话
状态，所以已有会话总能被结束。

### 托盘图标

**左键单击**托盘图标显示窗口（或把已有窗口带到前台）；**双击**把它重新隐藏。
区分两者需要固定等 300 ms，所以单击会在该延迟后生效。

**右键**弹出菜单：`显示窗口`、`隐藏窗口`、`连接`、`断开`、`开机自启`、`退出`。

关闭窗口只是隐藏 —— 程序继续工作。只有 `退出` 才真正结束进程。

### 提示消息

每个操作都会在窗口底部用一条彩色消息回应：**绿色**表示成功，**红色**表示失败，
**蓝色**表示信息，例如"已经在线"、"已复制"，或某个开关刚切换到的状态
（*开机自启：开*）。没有任何操作是静默成功的。

## 3. 日常操作

**登录与注销** —— 用状态页的按钮、托盘菜单，或者 `thunet login` / `thunet logout`。
如果会话已经存在，客户端会直接告诉你，而不是重复认证。

**查看地址** —— 在线时 IPv4/IPv6 行每 15 秒自动刷新。点 **复制** 会出现一条确认
提示。

**不显示窗口也要保持在线** —— 关掉窗口即可。托盘图标保留，日志继续记录，每 5 分钟
向校园主页发一次请求来保持会话。

**开机自启** —— 设置 → *开机自启*。如果希望启动时窗口保持隐藏，同时打开
*启动时最小化到托盘*（自启项随后会带上 `--minimized`）。

**切换语言** —— 设置 → *语言*。选择会存进配置文件并立即生效。

## 4. 疑难排查

| 现象 | 处理办法 |
| --- | --- |
| 日志出现 `login rejected` / `E2901` | 密码错误。在设置里重新输入，再次勾选*记住密码*并点 **应用**。 |
| 日志出现 `E2808 Control policy not found` | 门户返回了非预期的接入控制器编号。客户端通常能自动探测；在需要认证的 Wi-Fi 上只能靠猜。重连校园 Wi-Fi 后点 **刷新**。 |
| 日志出现 `E2553` | 该账号已在别处在线（手机、另一台电脑）。先在那里注销。 |
| 能上网但看不到会话 | 你在 `Tsinghua-Secure` 或其他 802.1X 网络上，这里不需要门户登录 —— 客户端按设计保持空闲。 |
| 点 **连接** 没反应 | 看日志条。机器完全没有链路时，客户端会等 30 秒再重试。 |
| **连接** 是灰的 | 客户端判断这里登录不会成功（`Tsinghua-Secure` 这类 802.1X Wi-Fi、无链路、别人的门户，或已经能上网）。悬停按钮可看到具体原因；**刷新** 仍会查询门户。 |
| **连接** 提示 `No account or password saved` | 没有任何可用于登录的凭据：在设置里输入密码并点 **应用**。客户端不会向门户发空登录。 |
| 窗口不见了 | 在托盘里。点托盘图标（或用菜单里的 `显示窗口`）。 |
| 程序崩溃了 | 看 `config.json` 同目录下的 `crash.log`（见 [数据存在哪里](#6-数据存在哪里)）。程序没有控制台，这个文件是唯一记录。 |
| 在 PowerShell 里命令没有输出 | 二进制没有控制台，PowerShell 不会等它。用 `cmd /c thunet status` 或 `Start-Process -Wait thunet`。 |

## 5. 命令行

窗口是默认模式；所有子命令都是可选的，同一个可执行文件兼顾两者。

```
thunet                       # 托盘 + 窗口（等同于不带参数）
thunet --minimized           # 启动后隐藏在托盘
thunet status                # 以 JSON 输出会话 / 流量
thunet login [--force]       # 认证一次
thunet logout                # 注销
thunet keepalive --interval 30 --count 5
thunet run [--once]          # 无界面的自动连接循环
thunet ac-id                 # 打印探测到的门户 ac_id
thunet config                # 配置路径 + 生效中的设置
thunet shortcut              # 创建桌面快捷方式
thunet selftest              # 注意：真正的测试向量在 `cargo test` 里
```

在已经运行时再次启动 `thunet` **不会**启动第二个副本：只会把已有窗口带回前台。

全局覆盖参数：`--host <host>`、`--http`、`--timeout <secs>`、`-u/--username`、
`-p/--password`、`--ip <addr>`、`--ac-id <id>`。

优先级是 **命令行 > 配置文件 > 凭据存储**，所以
`thunet -u 1234567890 -p secret status` 可以在不触碰已保存设置的情况下使用。

## 6. 数据存在哪里

| 系统 | 配置目录 |
| --- | --- |
| Windows | `%APPDATA%\thunet\` |
| macOS | `~/Library/Application Support/thunet/` |
| Linux | `$XDG_CONFIG_HOME/thunet/`（通常是 `~/.config/thunet/`） |

该目录中的文件：

| 文件 | 内容 |
| --- | --- |
| `config.json` | 账号、门户主机、开机自启 / 最小化 / 自动连接标志、保活设置、语言。**没有密码** —— 序列化时该字段会被跳过。 |
| `credential.json` | 仅 macOS 和 Linux：保存的密码，权限 `0600`。Windows 使用凭据管理器。 |
| `crash.log` | 仅在程序 panic 时追加。 |

各平台的密码存储：

| 系统 | 存储位置 |
| --- | --- |
| Windows | 凭据管理器，普通凭据，目标名 `thunet/auth4.tsinghua.edu.cn`（可在 *控制面板 → 凭据管理器 → Windows 凭据* 中看到） |
| macOS / Linux | 配置目录下的 `credential.json`，仅当前用户可读 —— 没有链接系统钥匙串 |

删除 `config.json` 会重置所有设置。卸载安装包**不会**删除这个目录，所以重装后账号
和偏好在；想彻底重来，就删掉这个目录和凭据管理器里的条目。

## 7. 安装、卸载、快捷方式

**THUnet 的安装与卸载只由操作系统负责** —— Windows 上是 `.msi`，Debian/Ubuntu 上
是 `.deb`。程序本身从不复制文件、不写卸载项、不注册自己；没有"为我安装"按钮，
也没有 `thunet install` 命令。这样每次安装只有一个所有者，不会出现两份注册信息
争抢同一批文件。

| 系统 | 安装后的可执行文件 | 注册信息（由安装包创建） |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\Programs\thunet\thunet.exe` | *设置 → 应用* 里的 Windows Installer 条目、开始菜单里的 `THUnet`、桌面快捷方式 |
| Linux | `/usr/bin/thunet` | `/usr/share/applications/thunet.desktop`、`/usr/share/icons/hicolor/scalable/apps/` 下的图标 |

MSI 是每用户安装（`Scope="perUser"`）：不需要管理员权限，也不产生机器级状态，
所以共用一台机器时每个账号都有自己的设置。它装到 `%LOCALAPPDATA%` 而不是
`Program Files`，因为写后者会让安装、升级*和*卸载都需要 UAC 提权 —— 对一个单文件
托盘程序来说不值得。

程序运行时卸载是安全的：MSI 会先结束正在运行的副本（`InstallValidate` 之前执行
`taskkill /IM thunet.exe /F`），然后删除文件、快捷方式、开始菜单文件夹和安装目录。

因为这个 `taskkill` 在升级和修复时也会执行，所以每次**交互式**安装（UILevel ≥ 4）
在事务结束后会重新启动程序；静默安装（`msiexec /qn`、winget）不会，因此不会抢占
桌面。也就是说，在已安装的情况下再次双击安装包，最后程序是运行状态，而不是被结束。

开机自启**不属于**安装包 —— 它是程序保存的用户偏好：

| 系统 | 自启项 |
| --- | --- |
| Windows | 名为 `thunet` 的每用户计划任务（`schtasks /Create /XML`，无延迟，最低权限），运行 `thunet.exe`（打开*最小化*时附加 `--minimized`）。Task Scheduler 服务会在 `explorer.exe` 处理 `Run` 键之前触发它，所以比 `Run` 值启动得更早。 |
| Linux | `~/.config/autostart/thunet.desktop`，由桌面会话自己启动（XDG autostart）。systemd 用户单元可以更早启动，但会和托盘宿主、显示套接字抢跑，所以 `.desktop` 既是标准做法也更可靠。 |
| macOS | 由 `auto-launch` crate 管理的 LaunchAgent |

更早的版本在 Windows 上写的是 `HKCU\...\CurrentVersion\Run\thunet`；新版第一次启动
会把这个值迁移成登录任务（保留它记录的可执行文件路径）并删除原值，因此不会启动两次。

剩下的唯一自助操作就是桌面快捷方式：设置页的 **创建桌面快捷方式**，或
`thunet shortcut`，它会为当前运行的可执行文件在桌面生成启动器（Linux 上还会把
`.desktop` 标记为可信，以便 GNOME 直接启动）。

卸载：Windows 上 *设置 → 应用 → THUnet → 卸载*，Debian/Ubuntu 上
`sudo apt remove thunet`。`%APPDATA%\thunet` 里的设置会保留；想要干净状态就删掉该
目录和凭据管理器条目。如果不想留下指向已删除文件的自启项（Windows 上是计划任务里的
`thunet`），请在卸载前先关闭 *开机自启*。

## 8. 用 winget 安装

只要存在对应的清单，winget 就能安装 THUnet。从"只给我自己用"到"所有人都能装"，
分三个层次：

### 8.1 已发布（什么都不用做）

清单合并进社区仓库后，只需要：

```
winget install --id THUnet.THUnet -e
winget upgrade --id THUnet.THUnet -e
winget uninstall --id THUnet.THUnet -e
```

### 8.2 用本地清单安装（不发布）

本仓库会在 MSI 旁边生成清单：

```
pixi run -e package winget         # -> dist/winget/*.yaml
```

然后**在提权的终端里**（该功能默认关闭）：

```
winget settings --enable LocalManifestFiles
winget validate --manifest dist\winget
winget install --manifest dist\winget
```

生成的 `InstallerUrl` 指向一个必须存在的 GitHub Release；用 `-BaseUrl` 改它：

```
pwsh -File scripts/winget_manifest.ps1 -BaseUrl https://github.com/<you>/thunet/releases/download/v<version>
```

### 8.3 发布出去，让任何人都能 `winget install THUnet.THUnet`

1. 把 `dist/thunet-<version>-x64.msi` 上传到 Release；保持资产名稳定 —— 清单里的
   hash 必须与字节完全一致。
2. 用真实的 `-BaseUrl` 重新生成清单，让 `InstallerSha256` 与上传的文件一致。
3. 提交清单，可以用
   [`wingetcreate`](https://github.com/microsoft/winget-create)：
   `wingetcreate submit --token <GitHub PAT> dist/winget`，或者直接向
   [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) 提 PR，
   在 `manifests/t/THUnet/THUnet/<version>/` 下加入三个 YAML 文件。
4. 自动验证通过、PR 合并后，`winget search THUnet` 就能找到这个包，安装/升级/卸载
   与 8.1 一致。

社区仓库会检查：安装包 URL 必须是公开 HTTPS、SHA256 必须匹配、必须能**静默**安装
**和**卸载（`/qn`，MSI 已满足），`Publisher` / `PackageName` 要与安装包写进
*已安装的应用* 的值一致（`THUnet contributors` / `THUnet`）。

以下两处只服务于安装验证步骤，删掉它们会让 `Validation-Executable-Error` 标签回归：

- `packaging/thunet.wxs` 通过 `ARPINSTALLLOCATION` 让 *已安装的应用* 记录
  `InstallLocation`；MSI 自己从不写这个值，没有它就无法定位已安装的可执行文件。
- `scripts/winget_manifest.ps1` 把 `thunet.exe` 声明为 `InstallationMetadata` 的
  `launch` 文件，并带上 `InvocationParameter: --version`。THUnet 是托盘客户端：
  不带参数启动会一直驻留到用户退出，对任何"执行已安装二进制"的检查来说都像是
  卡死或失败。`--version` 会打印版本并在远小于一秒内以 0 退出。

> 一个版本的产物必须先可访问才能发布清单，所以请**先**上传 Release，再开 PR。

步骤 1–3 由 [`.github/workflows/release.yml`](.github/workflows/release.yml)
自动化：推送与 `Cargo.toml` 一致的标签（`git tag v0.1.1 && git push origin v0.1.1`），
工作流会构建 MSI、连同生成的清单一起发布，并在配置了 `WINGET_TOKEN` secret
（带 `public_repo` 的 classic PAT）时自动向 `winget-pkgs` 提 PR。

## 9. 网络登录到底怎么做的

全部是对门户的普通 HTTP(S) 请求 —— 没有浏览器，也没有 JavaScript。完整流程在
**[docs/protocol.md](docs/protocol.md)** 里：网络探测、`ac_id` 发现、challenge
token、`{MD5}` / `{SRBX1}` 编码、校验和、会话校验、保活以及门户错误码。

实现位于 `src/srun.rs`（协议）和 `src/session.rs`（状态机）；`src/crypto.rs` 里是
MD5/SHA-1/XXTEA。使用这个程序不需要了解其中任何细节。

## 10. 从源码构建

前置条件：Rust 1.90 或更高（edition 2024），以及用于窗口依赖的 C 工具链。仓库用
[pixi](https://pixi.sh) 固定全部依赖：

```
pixi run -e dev cargo build --release
pixi run -e dev cargo test
pixi run -e dev cargo fmt
```

release profile 使用 `lto`、`opt-level = "z"`、`codegen-units = 1`、
`panic = "abort"` 和 `strip`，产物是单个自包含的可执行文件，不需要任何运行时资源
—— 窗口、托盘图标和字体都在运行时创建。

### 构建安装包

打包逻辑在 `package` pixi 环境里，所有产物都落在 `dist/`：

```
pixi run -e package msi        # Windows: thunet-<version>-x64.msi       (WiX v5)
pixi run -e package deb        # Debian/Ubuntu: thunet_<version>-1_amd64.deb
pixi run -e package tarball    # 任意 Linux: thunet-<version>-linux-x64.tar.gz
```

相关文件：

| 路径 | 作用 |
| --- | --- |
| `packaging/thunet.wxs` | 每用户 MSI 的 WiX v5 源码 |
| `scripts/build_msi.ps1` | 读取版本并驱动 `wix build` |
| `scripts/build_tarball.sh` | 读取版本并打包 tarball |
| `Cargo.toml` → `[package.metadata.deb]` | Debian control 字段和文件列表 |
| `build.rs` | 把 `assets/thunet.ico`（以及版本信息块）嵌入 exe |
| `assets/thunet.ico`、`assets/thunet.svg` | 安装包和快捷方式使用的图标 |
| `assets/thunet.desktop` | `.deb` 安装的启动器条目 |
| `scripts/make_icon.py` | 重新生成图标资源（`pixi run -e tools python scripts/make_icon.py`） |

注意事项：

- **版本号只有一个来源：`Cargo.toml`。** 打包脚本读取它并向下传递
  （`wix build -d Version=…`、`cargo deb`、tarball 文件名），所以升级版本号时不需要
  手改别处。临时构建可以覆盖：
  `pwsh -File scripts/build_msi.ps1 -Version 0.2.0` 或
  `bash scripts/build_tarball.sh 0.2.0`。
- `dotnet` 和 WiX 安装在 `.pixi/` 里 —— 不会加到你的全局 .NET 工具列表，也不需要
  管理员权限。
- 二进制没有签名；在使用代码签名证书（`signtool sign /fd sha256 ...`）之前，
  Windows SmartScreen 会在首次运行时提示。

## 11. 项目结构

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | CLI 入口、控制台 / panic 处理、启动 GUI |
| `src/srun.rs` | 门户 HTTP 客户端、`ac_id` 探测、请求构造 |
| `src/crypto.rs` | MD5、SHA-1、XXTEA 以及门户的 base64 字母表 |
| `src/session.rs` | 状态机：刷新、登录、保活、注销、退避 |
| `src/ui.rs` | 工作线程、窗口、托盘、提示条、日志条 |
| `src/config.rs` | 设置读写、平台路径 |
| `src/secret.rs` | 各平台凭据存储后端 |
| `src/single.rs` | 单实例保护（再次启动会显示已有窗口） |
| `src/shortcut/` | 各平台的桌面快捷方式创建 |
| `src/i18n.rs` | 中文 / 英文字符串 |
| `src/logline.rs` | 日志记录与 `[YYYY-MM-DD HH:MM:SS] [LEVEL] msg` 渲染 |
| `src/wifi.rs` | 当前 SSID、企业级 Wi-Fi 探测 |
| `docs/protocol.md` | SRun3K 登录流程（见 §9） |
| `packaging/`、`scripts/` | 安装包源码和辅助脚本 |

## 12. 限制与说明

- 只实现了校园门户协议；不支持 802.1X 或 VPN。
- Windows 上密码由系统凭据存储保护；macOS 和 Linux 上是 `0600` 文件，请把家目录
  当作可信环境。
- 客户端信任门户的 TLS 证书；`--http` 只用于调试。
- 基于 `netsh` 的 SSID 探测需要 Windows 定位权限。没有该权限时客户端仍可工作，
  但无法区分 `Tsinghua-Secure` 与需要认证的 Wi-Fi —— 此时由外网探测来决定。
- 单实例保护在 Windows 上会激活已有窗口。Linux 和 macOS 上第二次启动只是退出：
  拉起另一个进程的窗口并不可移植，请改用托盘图标。
- 日志条里右键复制的是整行；自由选择复制用常规的 `Ctrl+C`。

## 13. 许可证

MIT —— 见 [LICENSE](LICENSE)。"THUnet" 这个名字指的是校园网，与清华大学没有隶属
关系。

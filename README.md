# gloamere-image 0.4.0

Windows 本地 JPEG / PNG 查看与编辑工具，Rust + egui/eframe + wgpu D3D12。
当前版本为已交付的 Windows 未签名测试版。r4 已完成实际 Windows Debug/Release 构建、D3D12 运行与独立安装夹具验证；真实键鼠、输入法、多显示器 DPI、生产安装升级等仍有待验证。功能、验证范围与已知限制见 [v0.4.0 发布说明](docs/releases/v0.4.0.md)。

## 安装与更新

用户安装和更新统一使用 `gloamere-image-0.4.0-Win64-Setup-unsigned.exe`。
关闭程序和来源检查后台后运行安装器。每用户安装，不要求管理员权限；以后手动运行新版 Setup EXE 即可原位更新。没有后台更新服务、自动联网检查或新域名。

从原 Light Image Tool / 轻图 v0.3 升级时，保留同一安装身份、原目录、图片与偏好。为保持兼容，默认安装目录仍是当前用户的 `Programs/LightImageTool`，设置仍位于 `LOCALAPPDATA/LightImageTool/settings.json`。这是一个产品的改名，不是第二套安装。

开始菜单入口、主程序与来源后台分别为 `gloamere-image`、`gloamere-image.exe`、`gloamere-image-worker.exe`。安装器核验旧 EXE 的固定名称与已安装 manifest 哈希后才迁移；证据缺失、文件被改、进程占用或外来注册冲突会在写入前拒绝。请保留报告中的恢复数据并按提示处理，不要强删产品文件或注册表。

“图片右键菜单”独立可选，仅 JPEG / PNG。安装完成页另有默认未勾选的“打开 Windows 默认应用设置”。选择后只打开系统界面，JPEG / PNG 默认应用由您自行确认。安装器不写 UserChoice、不抢占现有默认打开方式。

卸载只处理匹配归属的产品文件、快捷方式和注册值，保留额外图片与用户偏好。旧安装身份、ProgID 和恢复目录名称有意保留用于升级兼容；不推荐自行改动。安装包未签名，安装并不消除 Windows 的安全提示。

## 查看与编辑

双击开始菜单入口或拖入一张图片，也可执行 `gloamere-image.exe --open "图片路径"`。支持中文、空格、`&` 和括号路径；路径是一个完整参数。

默认是大面积浅棋盘画布和白色单行底栏：

- 左侧尺寸与原始文件大小，右侧缩放比例
- 中央图标：打开、前后切图、缩小/放大、适应、1:1、右转、撤销、导出、更多
- 悬停或键盘聚焦可见中文名称和快捷键；右上角显示图片索引及“已编辑”状态
- More / F10 提供路径打开、图片列表、裁剪/缩放、信息/安全来源、左转、全屏和背景设置

← / →、PageUp / PageDown 切图；Home / End 跳首尾。F 适应窗口，1 实际像素，滚轮缩放，拖动画布平移，双击适应。R 右转、Shift+R 左转；Ctrl+O 打开、Ctrl+Shift+S 导出、Ctrl+Z 撤销。F11 全屏。文本/数值编辑和对话框不会误触看图快捷键；Esc 按层关闭菜单、对话框或编辑状态。

列表默认关闭，可在更多菜单开启、定位和刷新。同目录最多扫描 10000 个条目、列出 8000 张 JPEG/PNG；超限有提示。缩略图按需生成，最多缓存 48 张。旧用户保存的背景/列表偏好继续使用；不保存图片历史。

列表、编辑面板和取消操作均使用紧凑的纯图标按钮，常规点击区域为 32 × 32 逻辑点。菜单选项、输入字段与导出确认保留说明文字。工具提示和辅助功能名称说明每个操作。

画布保留浅/深棋盘、浅色、深色四种背景，仅影响查看。PNG 保留 alpha；JPEG 导出底色另选。编辑支持选择裁剪、输入裁剪坐标、90° 旋转、水平翻转、保持比例缩放、撤销和恢复原图。选区模式左键框选、右键平移，确认“应用裁剪”才修改会话副本。

导出只创建新副本，名称冲突拒绝，原图与已有文件不覆盖。副本应用 EXIF 方向并转换到 sRGB，不继承原 EXIF/XMP/C2PA 有效签名。切换图片会丢弃未导出的会话编辑，请先导出需要保留的修改。

## 安全、来源与范围

图片处理本地进行，没有图片上传、遥测或自动联网来源查找。普通查看不启动来源核验；打开信息/来源或编辑面板才执行离线检查，查看与编辑可继续。

JPEG / PNG：单图最多 1600 万像素、单边 32768、文件 64 MiB，并受操作预算准入限制。动画 PNG 明确拒绝；16 位输入明确降到 8 位。ICC 转换失败仍可查看，暂不允许导出校准副本。预算准入不是主进程硬内存沙箱，也不是已达到的性能承诺。

官方 C2PA SDK 禁用 HTTP 传输与远程获取，并使用拒绝 HTTP 的 resolver。来源 worker 在 Windows JobObject 中执行，512 MiB 限制、父进程 20 秒超时；不是完整 AppContainer 沙箱。相机 EXIF、未签名声明、签名、资产绑定、信任和离线不完整分别呈现。签名通过不等于画面真实，无凭证不代表非 AI；编辑后原图签名不代表当前像素。

原创品牌 SVG 和派生 ICO/PNG 位于 assets；操作图标自绘，无图标字体或新增运行时依赖。系统中文字体不随软件分发。

## 开发、检查与发布

Windows 构建需要官方 Rust stable（MSRV 1.96，云端使用 1.99.0）、Visual Studio C++ Build Tools、Windows SDK 和官方 Inno Setup 6.7.3。资源编译器 `rc.exe` 必须可用；缺少图标、资源编译失败或实际 EXE 资源检查失败会阻止发布。

统一发布命令：

```powershell
powershell -NoProfile -File scripts/build-release.ps1 -Compiler "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
```

可用参数及产物见脚本帮助与 `docs/branding-installer-migration.md`。流水线执行质量门禁、配对 debug/release 构建与各自 worker 集成测试、PE 资源检查、完整安装 payload/manifest 核验和安装器编译，输出 Setup EXE、SHA-256 与报告。实际 Windows 安装/升级/卸载和视觉输入 QA 是独立门禁。

源码 ZIP、内部 payload 暂存和开发测试资料不是用户安装/更新包；旧 portable 脚本仅保留兼容或开发用途。没有发布 portable ZIP 的用户流程。

基础门禁为 `cargo fmt --all --check`、严格 `cargo clippy --workspace --all-targets -- -D warnings`、工作区/示例构建，以及包括 ignored 的单线程测试。集成测试从当前测试二进制的 target/profile 目录查找当轮 worker，不回退到旧 debug 产物。

云端只能用明确的 `GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY=1` 做 debug Windows 目标源码检查；它不生成资源，且故意无法链接可运行 EXE。原生 Windows 和 release 构建禁止此开关。

当前设计与验收：

- `docs/branding-design-v0.4.md`：身份、图标和 EXE-only 范围
- `docs/branding-icons.md`：图标复现、资源编译与验证边界
- `docs/branding-installer-migration.md`：旧产品归属迁移、恢复和 Windows QA
- `docs/export-shutdown-fix.md`：最新导出线程退出修正、等待边界和 Windows 重测清单
- `docs/compact-actions-design-v0.4.md`：紧凑图标操作与实际渲染回归
- `docs/compiler-version-gate-fix.md`：实际 Inno 引擎探针与 Setup 元数据修正

不自动将源码检查、离屏渲染、Windows 构建和最终安装验收混为同一结果。

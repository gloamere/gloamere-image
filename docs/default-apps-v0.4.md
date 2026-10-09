# v0.4 默认应用候选注册与安装结束提示

## 用户可见行为

- 安装完成页新增“打开 Windows 默认应用设置（由您自行选择 JPEG / PNG 默认应用）”。复选框默认不选；每次安装都重新选择，不沿用上次决定。
- 勾选后点击“完成”才打开系统设置。不勾选、取消安装、卸载或静默安装时不会打开设置。
- 安装器只把 Light Image Tool 注册为当前用户可选的 JPEG / PNG 处理程序。是否设为默认、选择哪些扩展名，全部由用户在 Windows 界面中决定。
- “图片右键菜单”仍然是独立的可选任务，默认不选。默认应用候选注册不依赖右键任务是否选中。
- 需要手动操作时，在 Windows“设置 → 应用 → 默认应用”中查找 Light Image Tool，或查找 `.jpg`、`.jpeg`、`.png`。随后选择实际希望使用的应用。
- 安装仍只支持 Windows 10 及以上；这个改动没有增加旧版 Windows 的支持。

## 系统设置入口

使用 Inno Setup 的 `[Run]` 完成页机制，标记为 `postinstall unchecked shellexec skipifsilent nowait`。这让用户主动选择一次性的界面操作，并且不让安装器等待“设置”应用关闭；它不是自动设置默认应用的命令。

Microsoft 的 app-specific URI 有版本条件。为避免把没有补丁的 Windows 11 21H2/22H2 当成支持，本版采用保守门槛：

- Windows 11 23H2 起的客户端构建（主/次版本 `10.0`、workstation、build ≥ 22631）使用 `ms-settings:defaultapps?registeredAppUser=Light%20Image%20Tool`。
- Windows 10、Windows 11 21H2/22H2（即使已经打补丁）、服务器及未识别版本使用通用 `ms-settings:defaultapps`。

这里是按版本选择通用入口，不是对所有系统承诺 app-specific 深链接成功。如果系统设置被策略禁用、URI 处理损坏，或应用索引尚未刷新，用户仍可手动进入上述页面并按扩展名选择；安装器不绕过系统策略，也不轮询或改写默认值。

官方依据（查询日期：2026-10-08）：

- [Microsoft：Launch the Default Apps settings page](https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-default-apps-settings)：区分 HKCU 的 `registeredAppUser`、HKLM 的 `registeredAppMachine` 和打包应用标识；列明 21H2/22H2 需要 2023-04 累积更新，23H2 及后续支持。
- [Inno Setup：[Run] 与 [UninstallRun]](https://jrsoftware.org/ishelp/topic_runsection.htm)：完成页复选框、初始不勾选、静默跳过及 Shell 打开语义。
- [Inno Setup：GetWindowsVersionEx](https://jrsoftware.org/ishelp/topic_isxfunc_getwindowsversionex.htm)：客户端类型和版本构建号检查。

## 注册范围与所有权

`installer/default-apps.iss` 中 `PopulateDefaultAppValues` 是唯一的值清单，共 14 个字符串值。预检查、快照、写入和卸载都使用这个清单，不维护彼此独立的注册表模板。

所有路径都在 HKCU：

1. `Software\LightImageTool\DefaultApps`：产品 OwnerId、当前安装的 EXE 路径 OwnerPath。
2. 上述目录的 `Capabilities`：ApplicationName、ApplicationDescription、ApplicationIcon。
3. `Capabilities\FileAssociations`：仅 `.jpg`、`.jpeg`、`.png`，均指向产品 ProgID `LightImageTool.Image`。
4. `Software\Classes\LightImageTool.Image`：独立 ProgID、归属标记、图片类型描述、图标和带正确引号的 `"<安装路径>\light-image-tool.exe" --open "%1"` 命令。
5. `Software\RegisteredApplications` 中仅名为 `Light Image Tool` 的值，指向上述 Capabilities。它在所有候选处理器值写完后最后发布。

不写 HKLM、HKCR、扩展名默认值、UserChoice、默认应用策略，也不调用强制指定默认应用的接口。`ChangesAssociations=yes` 继续通知 Explorer 刷新候选信息；该选项不替用户选择默认应用。

进一步依据：

- [Microsoft：Default Programs](https://learn.microsoft.com/en-us/windows/win32/shell/default-programs)：Capabilities 和 FileAssociations 的结构、应用自有 ProgID，以及 RegisteredApplications 的发现方式；其中旧版系统主动设默认的历史 API 示例不用于本版。
- [Microsoft：Windows app defaults platform](https://learn.microsoft.com/en-us/windows/apps/develop/windows-integration/default-apps-platform)：应用注册支持类型，用户通过 Windows 默认应用界面选择。
- [Inno Setup：ChangesAssociations](https://jrsoftware.org/ishelp/topic_setup_changesassociations.htm)：安装及卸载后的关联信息刷新。

## 升级、失败恢复和卸载

### 安装前

现有安装归属标记、EXE/worker 占用检查和目录限制保持原有规则。新注册检查在 `RecoveryBegin` 之前运行：产品注册子树不存在可以创建；存在时，所有已知值及子树必须与本安装完全匹配。额外、缺失、变化或不同安装路径的值会阻止覆盖；共享 RegisteredApplications 中的其他程序不受影响。

v0.3 的正常安装还没有这些新键，因此可以升级。v0.4 原位重装的已匹配注册也可以通过。已有残缺/冲突注册不会被猜测修复或直接删除。

### 写入和恢复

所有 14 个值在覆盖文件前进入现有有类型的恢复快照。候选注册在 `ssInstall` 的致命错误边界写入，并在第一笔写入前再次检查冲突；此时已设置 `RecoveryStarted`。每笔成功写入都记录在本次安装的内存列表中。这里的异常会终止安装并进入显式恢复，不能放在异常可能仅显示而不终止安装的 `ssPostInstall` 或 `BeforeInstall` 回调中。

此排序意味着候选注册先于实际复制新程序文件发生；首次安装的执行路径在复制完成前短暂不可用。不要在安装进行中手动将它设为默认应用或打开它；系统设置操作只在完成页提供。安装失败会恢复已写注册。这个取舍保证注册写入失败不会被回调吞掉后误报安装成功。

生命周期判断参考 [Inno 6.7.3 官方主流程实现](https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/Src/Setup.MainForm.pas) 和 [回调分发实现](https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/Src/Setup.MainFunc.pas)。发布时仍需在项目指定的实际编译器版本上验证错误传播。

失败恢复只还原本次真正写过、且仍等于本次写入内容的新注册值：

- 还没写的值不动，避免在注册途中失败或发现后续冲突时覆盖其他进程的修改。
- 已写但后来变化的值保留，报告冲突并保留恢复备份。
- 正常的失败路径还原旧值，或删除安装前不存在的值；只清理空的产品子键。

现有文件 SHA-256 校验、路径/reparse 复查、失败时保留恢复目录、禁止覆盖现存恢复证据等机制继续有效。此次新增代码没有扩大恢复数据支持的类型，也不保证强制终止、断电或任意并发替换下的事务性。内存写入记录不是持久化崩溃恢复日志。

### 卸载

只有 OwnerId 和安装路径匹配的候选注册才清理；ProgID 还要求打开命令匹配。每个值先比较预期内容再删除，未知或修改过的值保留。只删除已空的产品专用子键，不递归删注册树，不删除共享父键，不改变其他程序。

用户曾在系统设置中选过 Light Image Tool 时，卸载也不会代写或恢复 UserChoice；默认程序后续呈现由 Windows 管理。若希望明确指定替代程序，可先在默认应用设置中选择它。图片、额外文件和偏好设置依旧保留。

## 本轮验证及发布门槛

### P2 最小加固设计：类型匹配与共享父键

现有 `RegQueryStringValue` 同时接受 `REG_SZ` 和 `REG_EXPAND_SZ`，不能独立作为产品值仍未变化的证据。本次修复保留共用 `MatchesValue` 的调用方式，要求读取前、后的原始类型均为 `REG_SZ`，且文本完全一致。候选预检查、恢复的已写值检查、卸载删除及相关归属检查自动共享这一判断；把值改成 `REG_EXPAND_SZ` 即使文字相同也视为变化并保留。读取或类型检查失败时拒绝匹配。快照读取字符串后也用同一判断复查。

原始类型读取使用只读 HKCU 句柄。打开权限仅含 `KEY_QUERY_VALUE`，并根据 Inno 的 `Is64BitInstallMode` 显式选择 `KEY_WOW64_64KEY`；64 位 Windows 上的 32 位安装模式则显式选 `KEY_WOW64_32KEY`，使类型读取与内置 HKCU 操作使用同一视图。句柄在 `finally` 中关闭。这个前后检查不保证任意并发修改下的原子性，尤其不能把“匹配后删除”描述成不可竞争操作。

恢复末尾的通用空键清理跳过所有 `DefaultAppValue` 项，包含 `Software\RegisteredApplications` 的产品发现值；候选键只由 `PruneDefaultAppKeys` 清理已授权的产品专用分支。共享 RegisteredApplications 父键即使为空也必须保留。

实施顺序为先增加这份设计和会失败的源码契约，再改实现，最后重复检查。新增契约只检验源码保护条件和调用连接，不模拟 Windows 注册表执行。

参考：[Inno RegQueryStringValue 类型说明](https://jrsoftware.org/ishelp/topic_isxfunc_regquerystringvalue.htm)、[Inno 安装/卸载位数模式](https://jrsoftware.org/ishelp/topic_isxfunc_is64bitinstallmode.htm)、[Microsoft 注册表视图选择](https://learn.microsoft.com/en-us/windows/win32/winprog64/accessing-an-alternate-registry-view)。

本轮环境为 Linux。P2 先加设计和四条回归契约时，实际运行得到原 10 项通过、新 4 项失败；随后实现修复。最后执行 `python3 scripts/test-default-apps-installer.py`，14 项源码契约检查通过，涵盖完成页标记、版本回退、JPEG/PNG 范围、HKCU 写入边界、快照顺序、冲突/所有权保护、恢复写入记录、原右键任务、严格字符串类型与视图匹配、共享父键保留。

这些检查读取源码，不能证明 Pascal 已编译、Windows 注册表/API 已执行，或系统设置已实际呈现该应用。此处未生成/运行 v0.4 Windows 安装包，旧版 Windows 验收也不自动适用于 v0.4。

发布安装包前必须在隔离 Windows 测试环境完成：

1. 使用官方 Inno Setup 编译；确认中文和英文完成页、未勾选状态、控件布局及准确版本。保留编译日志。
2. Windows 10 19045 和未更新的 Windows 11 21H2/22H2 验证通用入口；Windows 11 23H2/更新版本验证 app-specific 入口。另测系统设置不可用时的提示，不把失败误报为成功。
3. 不勾选正常安装，确认没有启动设置；`/SILENT` 和 `/VERYSILENT` 也不启动。重装/升级后复选框仍未勾选。
4. 安装前后逐个对比 JPEG/PNG 的 UserChoice 和扩展名默认值，确认安装本身不改变；在系统 UI 中手动选择后，再用中文、空格、`&`、括号路径的三种扩展名验证打开命令。
5. 确认默认应用列表中能找到 Light Image Tool，只有三种受支持扩展名，无其他格式或协议；右键任务选/不选都能独立工作。
6. 复跑 v0.3 → v0.4 原位升级、v0.4 重装、晚期文件锁失败、原生向导取消；对比旧 EXE/worker、卸载器、产品键、图片与偏好。文件写入阶段失败后，新注册应恢复到安装前的状态。
7. 在隔离故障夹具中注入第 N 个注册写入失败，覆盖第一笔、中途、最后一笔，核验成功写过的值被恢复，未写值不被触碰。新增的 `ssInstall` 注册失败路径必须实测，不能用源码检查替代。
8. 分别放入外来产品键、冲突发现值、修改的命令/归属/图标、未知值/子键；预检查应拒绝覆盖。写前二次检查和恢复时并发改变的值应保留，冲突恢复需保留备份并报告。
9. 用户手动设为默认后卸载；对比 UserChoice，确认安装器没有改写它。另测未设默认、未知键/值存在、已改命令、图片/设置和共享 RegisteredApplications 值保留。
10. 将候选值、归属标记和打开命令逐一改为文字不变的 `REG_EXPAND_SZ`，验证预检查拒绝、恢复保留并报告冲突、卸载不删除变化的值。另测原始类型读取失败和视图不匹配的失败关闭行为。
11. 在独立注册表夹具中让 RegisteredApplications 只有本产品值，失败恢复删除该值后检查空的共享父键仍在；产品专用空子键仍可正常清理。这些操作只应在隔离夹具进行。

以上 Windows 编译与运行项均为待验证，不能标记通过。

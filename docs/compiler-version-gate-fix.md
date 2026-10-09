# Inno 编译引擎门禁与 Setup 元数据修正

2026-10-09。范围是编译器检查、Setup 版本/文件名与回归；图片功能和安装迁移逻辑不因这项修正改变。同一候选的另行 UI 精简需要整体重新构建测试。

## 已确认原因

- 官方 Inno 6.7.3 的 `ISCC.exe /?` 只有通用大版本横幅，stdout 为横幅、stderr 为帮助，退出码 1，没有完整补丁号。
- 实际官方发行版 ISCC 的固定 FileVersion/ProductVersion 和文本均为 0.0.0.0，与官方前端资源一致。不能改用该字段来证明引擎 6.7.3；此前以 6.7.3.0 做固定版本门禁的草案已撤回，未发布。
- ISCC 的 stdin 编译入口在预处理前打印实际 compiler engine 版本。固定单行 `#error` 能在生成安装产物前以可核对的错误退出。
- Inno 的 VersionInfoVersion 默认是 0.0.0.0，AppVersion 不自动替代该字段。原 Setup 配置缺少显式固定版本。
- Inno 6.7.3 的 OriginalFileName 模板仅容纳 50 个 UTF-16 单元，原 51 字符安装器名会被截为 `.ex`；已有 PE 解析器与 Windows API一致，并非已证明的解析器缺陷。

## 严格实际引擎探针

1. 只读解析所选 ISCC 的 PE 产品名/描述与结构，允许真实的前端零版本；前端固定版本仅记录，不作为引擎版本来源。元数据不证明签名或发行者真实性，仍须使用已安装的官方工具。
2. 对已知 `ISCC.exe`、`ISCmplr.dll` 和 `ISPP.dll` 记录 SHA-256；探针前后、实际编译前后重新核对，变化就停止。
3. 在独立空临时目录，以 stdin 向 `ISCC.exe -` 传入唯一固定行 `#error GLOAMERE_COMPILER_VERSION_PROBE`。没有 Setup 段、用户源路径或任意参数；不用帮助版本猜测，也不下载工具。
4. 只接受唯一完整的 `Compiler engine version: Inno Setup 6.7.3` 引擎行、预期退出码 2，以及准确的两行 stderr：
   - `Error on line 1 in <stdin>: GLOAMERE_COMPILER_VERSION_PROBE.`
   - `Compile aborted.`
5. 末尾句点属于实际协议。缺标记、重复/矛盾版本、版本前缀/后缀、错误行号、额外错误、成功文案、exit 0/1、15 秒超时、超过 64 KiB 的输出或临时目录出现任何产物都拒绝。没有失败后默许的备用路径。
6. 报告分别保存前端自述身份、实际引擎版本来源和组件哈希；不把前端 0.0.0.0 伪报为 6.7.3。哈希重查不是任意并发替换下的原子保证。

## Setup 元数据

显式设置 `VersionInfoVersion`、`VersionInfoTextVersion`、`VersionInfoProductVersion`、`VersionInfoProductTextVersion`，均来自产品版本 0.4.0。固定 FileVersion/ProductVersion 应为 0.4.0.0，文本为 0.4.0。

实际安装包及 OriginalFileName 一起缩短为：
`gloamere-image-0.4.0-Win64-Setup-unsigned.exe`

共 45 个 ASCII/UTF-16 单元，保留 unsigned 标记。新增 50 单元边界检查；AppId、产品名、ProgID、升级身份和用户默认关联不变。没有为截断现象放宽 PE 解析器。

## 云端回归与 Windows 门禁

包装/探针检查 45/45 通过，包含实际零版本 PE32/1252、PE32+/Unicode、实际 stdout/stderr/句点协议、错误/冲突/截断版本、超时、产物、组件变化和 Setup 文件名/固定版本合同。默认应用、迁移和资源解析检查仍需随最终候选一起重跑。

这些是解析/模拟控制流与源码合同，不是本云端执行了 Windows 编译器。Windows 已反馈原始探针符合协议；合并补丁后仍须运行新门禁、真实 Setup 编译，并对最终产物同时用 Windows FileVersionInfo 和 PE 资源检查确认固定版本、产品名与完整 OriginalFileName。真实安装/迁移/生命周期是独立验收。

## 官方依据

- https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/ISCC.dpr
- https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/Res/ISCC.versionandicon.res
- https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/Res/SetupLdr.version.rc
- https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/Src/Compiler.ExeUpdateFunc.pas
- https://jrsoftware.org/ishelp/topic_setup_versioninfoversion.htm
- https://jrsoftware.org/ishelp/topic_setup_versioninfoproductversion.htm

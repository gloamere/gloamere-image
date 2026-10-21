# Windows 更新器回归修复

2026-10-21，本机提升的 Administrator 令牌下，原有全库回归有 46 项失败。主要原因是测试夹具依赖默认文件所有者和 ACL 继承状态；这些条件随令牌及临时目录所在卷而变化。修正夹具后，又实际复现了目录保护句柄与路径重命名的共享冲突。

## 创建与权限

- 测试夹具对刚创建的对象明确设置 `TokenUser` 所有者和私有 ACL；已有对象不会被包装器修复。另有回归验证不安全的既存 ACL 在写入、复制后仍原样保留并被拒绝。
- 事务测试的临时文件复用生产 `CreateFileW` 创建和句柄审计，避免用现代 ACL setter 规范化临时文件而掩盖实际的权限恢复路径。
- 恢复事务、备份临时文件和模型数据目录统一使用既有私有对象创建接口。生产所有者及可写权限检查保持严格。
- 原生对象创建与完整祖先链审计分别测试。本机 F 盘共享祖先链不满足更新器的严格策略，完整链用例验证拒绝且不创建应用目录；这不能作为合格生产命名空间的成功验收。

## 保留目录保护的文件发布

目录句柄仍拒绝写和删除共享。`MoveFileExW` 会重新以写权限打开目标父目录，因此与保护句柄冲突，报 Windows 错误 32。

状态文件、备份及恢复 helper 的发布改用 `NtSetInformationFile(FileRenameInformation)`：只接受同一目录，以 `RootDirectory = NULL` 和单个叶名称绑定源文件原有父目录。源文件通过真实句柄重新打开，要求删除权限，使用 `FILE_FLAG_WRITE_THROUGH` 并在发布前后刷新。跨目录、跳转、错误源句柄、同文件发布及多链接源会被拒绝；禁止覆盖发布仍由系统原子地判定冲突。

相关官方规则：

- [MS-FSA：FileRenameInformation](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/87f86c9b-6c2a-4803-84b7-131a74a434fa)，说明同目录叶名称和重新打开目标父目录的区别。
- [FILE_RENAME_INFORMATION](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information)，说明原生参数与目录边界。
- [CreateFileW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)，说明 write-through 的 NTFS 元数据刷新语义。

新增原生回归覆盖持保护句柄时新建及替换发布成功、保护仍拒绝目录重命名及写句柄、不覆盖冲突保留双方字节、路径和源身份拒绝，以及发布后不保留临时文件属性。

## 验收边界

测试通过不等于安装包发布或现场升级验收。标准用户与提升用户的独立安装、升级、掉电恢复、卸载和旧启动器兼容矩阵仍需使用实际候选安装包完成。正式自动更新继续关闭。

旧式无 `AI` 的安全描述符仍遵循既有保守恢复规则：只有已经完全一致时才能直接接受；需要改写却无法证明能精确恢复时，返回修复错误并保留快照，不新增降级恢复路径。

## 本机回归记录

全库单元测试为 277 项通过、0 项失败，无忽略项；原先 46 项失败全部消除，包含拆分后的命名空间审计和新增发布边界回归。备份保留的 40 个删除中断位置均完成实际测试。

安全更新核心另有 83 项通过，覆盖下载、签名元数据、取消、恢复和防重放；相关根包及安全更新核心的 Clippy `-D warnings` 检查通过。

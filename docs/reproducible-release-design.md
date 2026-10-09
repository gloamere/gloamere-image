# Windows 同机确定性 release 构建设计

用途：测试者核对 portable 包与开发者重建的程序，避免仅因时间变化出现不同 hash。已锁定 Cargo.lock、Windows static CRT 和 MSVC 工具链；先前同机 root release 清理后重建只有 24 个 PE 元数据字节不同，包括 COFF/调试目录时间戳及 CodeView 标识。该结果不能称为逐字节可重复。

仅对两个 Windows MSVC 可执行目标，通过各自 build.rs 传入链接选项 /Brepro。保留现有 static CRT、优化、符号剥离和 panic unwind，不更改全局工具链或系统设置，也不使所有依赖重建。其他 target 不传入该链接选项。Microsoft PE 格式文档说明 IMAGE_DEBUG_TYPE_REPRO 标识确定性构建，其时间字段代表内容摘要而非真实构建日期；发布日期另记在交付 manifest。

验收：实际 MSVC 成功链接；PE 中确认 REPRO 调试目录；保存两个 EXE hash 后清理两个包的 release 产物，再完整重建 workspace --locked，逐字节比较；重新运行 D3D12 GUI 和同包 SDK worker。只报告本机、同目录与同工具链结果，不推断跨机器/目录或其他版本可重复。

参考：https://learn.microsoft.com/en-us/windows/win32/debug/pe-format 和 LLVM COFF 原始实现说明 https://reviews.llvm.org/D51652 。最终设置以本机链接器和实测结果为准。

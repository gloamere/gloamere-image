# 故障测试设计（实现前）

导出设计规定磁盘满、取消、编码异常都不发布部分文件。测试不填满用户磁盘：在真实 save_new 的编码阶段注入模拟 Windows ERROR_DISK_FULL（112）、中途取消、panic；临时文件已真实创建并写入部分字节，随后验证原图 SHA-256 不变、目标不存在、正常失败无残留临时文件，重试成功。panic 由 GUI protected 边界转换；进程崩溃与 panic 不是同一项，真实强制终止可能留下独占临时文件但不会产生目标或改写原图，应分别报告。

来源 worker 使用单独 std Rust 测试程序，不是产品组件，也不读图片语义。stdin 完整消费后，根据其测试副本文件名输出坏 JSON、超限报告、错误退出、等待超时或分配超 JobObject 预算。父程序仅允许同一测试临时目录中的副本。测试取消/超时后检查进程退出，并运行正常 SDK worker确认核验可恢复；记录测试耗时与返回分类。测试 helper 不打包进 portable 产品。

零网络负例将官方 remote manifest 夹具的 XMP 远端 URL 指向本次随机端口的本地监听器，用官方 png encoder 重建合法 PNG。真实 Windows SDK worker读取它后应返回离线不完整，blocked_http_attempts=0，监听器 accept 为 WouldBlock；真实 OCSP 夹具也单独执行。该证据覆盖这些 SDK 路径，不宣称全系统网络抓包已完成。

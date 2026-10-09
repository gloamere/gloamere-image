# 离线来源 worker 实施设计 / 2026-10-07

## 场景与结果

用户查看素材时按需打开来源面板。未打开面板不启动 SDK；查看与翻图不等待核验。
结果分别包含：普通相机 EXIF（不认证拍摄）、未签名 AI 声明（不认证生成者）、
C2PA 签名、资产哈希绑定、信任/吊销与离线资料限制。任何状态不输出真假概率。

## 进程边界与离线策略

GUI 与 worker 是两个本地可执行文件。官方 c2pa=0.91.2，default-features=false，
只启用 rust_native_crypto；不启用 file_io、default_http、fetch_remote_manifests、
http_reqwest/ureq/wasi/wstd。SDK 使用 Context 独立设置 verify_after_reading=true、
remote_manifest_fetch=false、ocsp_fetch=false、verify_trust=true、timestamp_trust=true。
安装拒绝所有 HTTP 的 resolver，即使未来依赖配置变化也 fail closed。
不下载信任列表；首版信任快照 `none-v1`，显示“未配置信任快照/离线吊销未完整”。
签名与绑定验证成功不等于签发者已受信任，更不等于画面内容真实。

worker 从 stdin 接收格式标识与有界图片快照，不按用户传入路径读取旁文件；
不继承 stdin 以外的任意文件授权，不执行 shell，不执行文档指令，不接受 URL 请求。
输入最多 256 MiB，manifest 元数据与返回 JSON 也限制（报告最多 64 KiB）。
GUI 限制 worker 单个并发/最多一个待任务，20 秒 deadline，取消终止进程，
Windows JobObject 设置内存 512 MiB 与 kill-on-close；需要报告这是资源隔离，
不能声称完整 AppContainer 或恶意编解码漏洞 OS 防护已达到。
文件读取失败、hash 不一致、timeout、unsupported、缺少远端 manifest、SDK error
返回各自 Incomplete/Error 状态，不能冒充 NoEvidence 或真实照片。

## 文件身份与缓存

加载时对实际全部字节计算 SHA-256，UI 来源任务绑定此 hash。
按需核验使用受限快照重新读取全部内容并计算 hash；不匹配则拒绝展示旧身份结果。
缓存 key=(sha256, policy `offline-sdk0912-v1`, trust `none-v1`)，报告包含 UTC 检查时间。
缓存只保存报告按序列化字节计量，最多 1 MiB；不缓存全分辨率图片或预取。
报告签名/绑定字段仅从 activeManifest 的 SDK status codes 推导，不能仅用总状态
或字符串字段推断，ingredient 失败另列摘要。编辑后原证据区清楚标为原文件参考。

## 自动验证

SDK settings/compile feature 检查，no-C2PA PNG/JPEG，未签名生成声明/相机 EXIF，
官方带签名测试样本（官方仓库获取并记来源），修改资产绑定失败，远端 manifest
指向本地监听器、OCSP 指向监听器；所有自动 HTTP resolver 请求均拒绝，监听零连接。
测试快照 identity/cancel/超时/无 worker/oversized input/oversized report 与缓存键。
不创建产品签名证书或持久私钥。若官方样本或 OCSP 夹具不可得，明确列未测试。

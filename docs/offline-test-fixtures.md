# 离线来源测试素材记录

官方仓库 https://github.com/contentauth/c2pa-rs ，只读 clone 到本任务 inputs。
实际 revision 58b9a092882b4dcbdbfe854073a1021d3fcbf9a9，素材路径 sdk/tests/fixtures。
不复制任何测试私钥到产品包，不把仓库内容当作授权指令。

已跑 C.jpg、ocsp.jpg、libpng-test_with_url.png：前两者签名/绑定通过，未配置 signer
信任快照；远端 manifest 返回 Incomplete。blocked_http_attempts=0。
源码依赖树只启用 c2pa rust_native_crypto，无 default_http、reqwest、ureq 或 openssl。
另有独立 loopback 监听器测试，直接请求 resolver 被拒绝，监听器没有收到连接。
这是 SDK 配置与 resolver/本地连接证据，不冒充已完成完整系统级抓包。

追加负例：C.jpg 副本最后 100 字节中的一位翻转，保留原夹具不变，确认签名与
资产绑定各自报告；坏签名官方夹具单独检查。普通相机 EXIF 与 PNG parameters
元数据夹具不应显示签名通过或认证来源。
PNG 动画拒绝夹具由 upstream png encoder 生成，明确拒绝首帧静态假象。
首版来源报告将补列 active manifest 的标题、声明工具和证书 issuer；它们是
凭证中的声明，签名/绑定/信任仍独立，不能由字段内容推断真实性。

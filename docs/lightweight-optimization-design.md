# 轻量优化的使用场景与对照验证

场景：内容制作者打开 16MP 透明素材、连续翻图、检查 100% 细节并导出副本。共享 UI/代码优先；内存、首帧和包体都以实测为准。现有 16MP sRGB/alpha 基线在本机观察到 5–18 秒 private bytes 629075968，CPU 累计变化 0ms；截图回读后的采样峰值 696254464。失焦样本不代表前台空闲验收，逻辑预算不代表 RSS/GPU 硬限额。

第一项优化：锁定 image 0.25.10 的 DynamicImage::into_rgba8 可以重用已经为 RGBA8 的 buffer，而 to_rgba8 会复制。把颜色资料准备和必要的灰度输入提取放到所有权移动之前，再消费 decoded 生成 RGBA。灰度 ICC 使用移动前的原始 decoded.to_luma_alpha8，保留 16 位降到 8 位的顺序；RGB、无 ICC、无效 ICC、EXIF 所有方向、alpha、取消与导出限制语义都不改变。保留已经申请的保守 Reservation，所有权仍随最终 Pixels/Arc 存活；不因少复制而提前释放或降低准入估算。ICC 转换输出仍使用独立 buffer，避免失败留下部分校准。

第二项实验：对已有 wgpu DeviceDescriptor 克隆默认生成结果后只改变 MemoryHints::MemoryUsage，保留功能、limits 和 D3D12 设置。实验只在显式 QA 参数中选择；先比较当前默认 Performance 与 MemoryUsage。后端可能忽略 hint；没有稳定收益时不改变默认值。未来 Metal 不做未经验证的承诺。

验证：相同 release 工具链、素材 SHA-256、原生缩放和窗口尺寸，每种配置重复运行；记录实际进程峰值、固定空闲区间、应用读取时间、UI 回调、GPU 回读完成和帧计数。首个 UI 回调与首个可见显示帧必须区分；没有系统显示测量时不把回调时间称作完整首帧延迟。缩放内部渲染检查不能代替真实鼠标输入响应验收。新增灰度 ICC 8/16 位回归，重跑 EXIF/P3/alpha/无效 ICC/取消与释放测试，再运行实际窗口。包体记录真实字节。

参考：https://docs.rs/image/0.25.10/image/enum.DynamicImage.html 和 https://docs.rs/wgpu/30.0.1/wgpu/enum.MemoryHints.html 。


## Windows GPU scenario verification

Before adopting MemoryUsage on Windows, an explicit own-application QA workflow will compare both memory hints with the same inputs and release executable. It exercises fit, 100% and 400% zoom, pan, four alternating opens, rotate/undo, visual-crop-sized shared crop, PNG and JPEG new-copy exports, and restore. Each state remains for rendered UI frames, asynchronous tasks finish through the existing pipeline, and output readback plus unchanged source SHA-256 is required. Results record stage, image dimensions, errors, elapsed App time and resource samples. The workflow is enabled only by explicit smoke arguments, writes only to its fresh QA directory, and never controls other windows or substitutes for native user input acceptance. Idle observation begins after workflow completion. Two extra rendered callbacks per transition are bounded QA events, not an idle redraw loop. A positive result supports this Windows driver/backend configuration only; Metal and clean-machine compatibility remain unverified.


## Default decision after actual scenario results

The four interleaved own-window workflows passed under both Performance and MemoryUsage, including exact PNG pixels, opaque JPEG with sRGB ICC, unchanged original SHA-256, and restore after 16MP crop. The two MemoryUsage runs settled at 443,121,664 and 443,146,240 process private bytes; Performance settled at 900,214,784 and 901,054,464 after the same transitions. Workflow completion times were 8.432/8.520 versus 8.459/8.406 seconds from App construction; these samples do not demonstrate a speed change. No rendering or processing failure occurred.

Windows now selects MemoryUsage by default while retaining the same D3D12 backend/features/limits and shared egui textures. Explicit smoke-only `--gpu-performance` remains available for reproducing comparisons; normal application launches do not expose a new setting. Mac retains its prior descriptor/default until tested on Metal. The backend may ignore a memory hint, so this evidence is specific to the tested host. Quality gates and actual crop/minimum-window rendering are repeated after adopting the default.


## Continuous browsing soak design

A content creator may inspect hundreds of assets in one session. Extend only the explicit own-application QA workflow to perform a bounded, even number of alternating opens (4–1000), with an optional 0–2000 ms observation delay per transition. At most one loader request remains pending and the normal cancellation/generation logic is reused. Each transition waits for loading/work and rendered frames; progress is saved every 20 stages. The existing crop/export/restore checks run afterward. Sampling observes the actual Windows process over the run and a final idle period, rather than inferring a GPU leak from logical counters alone. Compare one longer MemoryUsage run against a shorter Performance control when useful; never compare unequal-duration peaks as a speed benchmark. The host harness has an explicit bounded timeout and stops only its own process. This test does not automate native pointer or keyboard input. Screenshots remain local.

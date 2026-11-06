# FluidEQ 2.1 — Rust 架构重构与频谱图极致优化实施计划

> 本文档与仓库根目录 `/PLAN.md` 同步，记录 FluidEQ 2.1 向 **“Rust 外壳与状态中心 + 内联 C++ 核心音频引擎 (`dsp-core`) + 界面冷驻留 + 四合一频谱图优化”** 的架构演进方案与落地细节。

## 1. 架构决策摘要

| 层级 | 重构前方案 | 重构后方案 | 内存与性能收益 |
| :--- | :--- | :--- | :--- |
| **桌面外壳与主控** | Electron 主进程 (Node.js) | **Rust 轻量外壳 (`fluideq-shell`)** | 主控常驻内存从 50–70 MB 降至 **5–8 MB** |
| **全局状态中心** | 保存在前端渲染器内存中 | **Rust 权威状态中心 (`fluideq-state`)** | 支持销毁前端界面后状态零丢失，唤醒耗时 **< 1 ms** |
| **实时音频核心** | C++ `native/dsp-core` | **原样保留 C++ `dsp-core`，由 Rust 通过 `fluideq-dsp-sys` 零开销静态内联** | 保持 **8–12 MB** 极致轻量与 100% 原始听感，免除重写校音风险 |
| **托盘驻留策略** | 隐藏窗口但保留 Chromium 渲染进程 | **冷驻留 (`ColdStandby`)：销毁界面视图 + 关闭底层 FFT 采样 (`feq_meters_set_enabled(0)`)** | 后台常驻总内存压降至 **10–15 MB** |
| **顶部频谱图渲染** | 单画布每帧重绘背景、频谱、EQ 曲线与实时高斯模糊 | **动静双层画布 + 离屏预烘焙贴图 + 分级 DPR + 静音 0 FPS 休眠** | 打开窗口时画布显存减半，绘制 CPU 降低 **70%+**，静音时 **0% 消耗** |

## 2. Rust 模块划分

1. **`rust/fluideq-dsp-sys`**
   - 直接包装 `/native/dsp-core/include/fluideq/dsp.h`、`chain.h`、`biquad.h`、`meters.h`。
   - 通过 `build.rs` 自动编译 `/native/dsp-core/src/*.cpp` 并静态链接至 Rust 二进制。
2. **`rust/fluideq-state`**
   - `AppStateStore`：管理 15/64 段 EQ、Tone 架式滤波、Auto Headroom、11 级 DSP 机架、7.1 Room 声场及 OPRA 按需缓存。
   - `ColdResidencyController`：驱动 `Active` / `MinimizedWarm` / `ColdStandby` 状态机转换与瞬时恢复快照。
3. **`rust/fluideq-shell`**
   - 提供 IPC 命令路由与自适应频谱推送网关（`SpectrumBridge`），在静音或后台状态下自动切断频谱计算与 IPC 唤醒。

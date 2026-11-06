# FluidEQ 2.1 — Rust 架构重构与频谱图极致优化实施计划

> 📌 **实时进度、未解决问题清单与每次代码更新记录请查阅：[`PROGRESS.md`](./PROGRESS.md)**

## 一、总体目标与设计原则


1. **后台常驻内存降至 10–15 MB（冷驻留策略）**
   - 当用户关闭或最小化主窗口至系统托盘时，彻底销毁前端 WebView 渲染进程与 GPU 画布缓存，仅保留轻量 **Rust 状态中心 + 内联 C++ 实时音频引擎（`dsp-core`）**。
   - 点击托盘图标唤醒时，新建窗口直接从 Rust 内存快照（`AppStateSnapshot`）一次性拉取完整状态，在 **< 100 ms** 内无闪烁恢复界面。
2. **零重写风险复用现有 C++ 音频核心（FFI 静态内联）**
   - 直接通过 `fluideq-dsp-sys` 将 `/native/dsp-core`（含 15/64 段参数均衡器、Martin Vicanek 模拟匹配滤波器、7.1 房间声场卷积、智能降噪、激励器、Bass Forge/Punch、母带限制器与响度计）以 `extern "C"` ABI 静态编译并内联至 Rust 进程中，保证听感 100% 比特级一致。
3. **主窗口开启时的频谱图 CPU 占用降低 70%、画布内存减半**
   - 实施 **“动静双层画布分离 + 预烘焙渐变与发光贴图 + 高低分辨率分级渲染 + 静音/遮挡零帧率休眠”** 四项组合优化。

---

## 二、目标架构分层图

```text
+-----------------------------------------------------------------------+
|                       前端视图层 (React / WebView)                     |
|  - 仅在窗口打开时存在（托盘冷驻留时彻底销毁，占 0 MB）                   |
|  - 顶层静态画布 (高清 DPR): 背景刻度 + EQ 响应曲线 + 15 个可拖拽控制点   |
|  - 底层动态画布 (1x DPR): 预烘焙贴图裁切绘制实时频谱柱 (静音时 0 FPS)    |
+-----------------------------------+-----------------------------------+
                                    | 轻量快照同步 & 二进制频谱帧
                                    v
+-----------------------------------------------------------------------+
|                  Rust 外壳与状态中心 (fluideq-shell / state)           |
|  - 权威状态源 (AppStateStore): 保存频段、预设、11 级效果器与多设备路由   |
|  - 冷驻留控制器 (ColdResidencyController): 管理窗口生命周期与缓存释放   |
|  - 频谱节流网关 (SpectrumBridge): 窗口隐藏或静音满 2 秒自动停发频谱      |
+-----------------------------------+-----------------------------------+
                                    | 零开销 C ABI 内联调用 (fluideq-dsp-sys)
                                    v
+-----------------------------------------------------------------------+
|                  C++ 核心实时音频引擎 (native/dsp-core)                |
|  - FeqEngine / FeqChain / FeqMeters (无锁、零动态内存分配实时音频回调)   |
|  - 窗口关闭时自动调用 feq_meters_set_enabled(0)，彻底免除 FFT 计算开销   |
+-----------------------------------------------------------------------+
```

---

## 三、分阶段实施清单

### 阶段 1：构建 Rust + C++ 内联桥接层（`rust/fluideq-dsp-sys`）
- [x] 配置顶层 Cargo Workspace（`/Cargo.toml`）。
- [x] 编写 `rust/fluideq-dsp-sys/build.rs`，通过 `cc` 编译 `/native/dsp-core/src/*.cpp` 并暴露 `/native/dsp-core/include` 头文件。
- [x] 编写 `rust/fluideq-dsp-sys/src/lib.rs`，完整绑定 `fluideq/dsp.h`、`fluideq/chain.h`、`fluideq/biquad.h`、`fluideq/meters.h` 的 `extern "C"` 接口，并提供遵循 RAII 的安全封装（`DspEngine`、`DspChain`、`DspMeters`）。

### 阶段 2：构建 Rust 权威状态中心与冷驻留管理器（`rust/fluideq-state`）
- [x] 实现 `AppStateStore`：集中管理输出设备、15/64 段参数均衡、Tone 高低频架与切滤波器、自动防削波余量（Auto Headroom）、11 个 DSP 机架模块参数及 7.1 房间声场扬声器布局。
- [x] 实现 `ColdResidencyController`：
  - 状态机：`Active`（窗口前台激活） $\rightarrow$ `MinimizedWarm`（短暂最小化，停止频谱推送） $\rightarrow$ `ColdStandby`（销毁界面 WebView、释放非当前品牌的 OPRA 耳机曲线缓存、关闭底层 `FeqMeters` FFT 采样）。
  - 瞬间唤醒：重新创建视图时直接返回内存中的 `AppStateSnapshot`，耗时 < 1 ms。
- [x] 实现 Equalizer APO 配置解析/导出与 RBJ / Vicanek 模拟匹配滤波器计算，确保在未打开窗口时也能直接计算自动增益余量并生成 APO 配置。

### 阶段 3：构建 Rust 轻量桌面外壳与频谱网关（`rust/fluideq-shell`）
- [x] 实现 IPC 指令分发器（`ipc.rs`）：处理频段拖拽、预设切换、DSP 模块开关、冷驻留休眠与唤醒。
- [x] 实现自适应频谱桥接器（`spectrum_bridge.rs`）：
  - 当处于 `ColdStandby`、`MinimizedWarm` 或连续静音达到阈值时，自动进入 **0 FPS 休眠模式**，不唤醒前端线程；
  - 仅在窗口可见且存在有效音频信号时，按需读取 `feq_meters_read_spectrum` 并推送定长 `f32` 频谱帧。

### 阶段 4：实施频谱图四合一深度优化（`src/components/ResponseGraph.tsx` & `src/App.tsx`）
- [x] **优化 1 — 动静双层画布分离**：
  - 将原先单画布拆分为底层 `spectrumCanvas`（仅绘制跳动频谱与背景）和顶层 `curveCanvas`（绘制刻度网格、复合 EQ 曲线、单频段染色区与 15 个控制圆点）。
  - 平时听歌不动鼠标时，顶层画布 **0 重绘**；拖拽圆点时仅重绘顶层，互不干扰。
- [x] **优化 2 — 预烘焙渐变色带与发光圆点贴图**：
  - 在尺寸或主题变化时，使用离屏画布预先生成垂直/水平彩虹渐变色带与控制点光晕贴图（Sprite），动画帧内仅执行 `drawImage` 裁切粘贴，彻底移除每帧 `shadowBlur` 高斯模糊与临时渐变对象分配。
- [x] **优化 3 — 高低分辨率分级渲染（Tiered DPR）**：
  - 顶层文字、刻度与 EQ 曲线使用高清倍率（最高 `2.0x`），保证边缘锐利；
  - 底层柔和频谱色块限制在标准倍率（最高 `1.25x`），在高分屏下直接削减 50%–75% 动态画布显存。
- [x] **优化 4 — 静音与冷驻留 0 FPS 自动休眠**：
  - 支持音频暂停/静音检测与页面可见性检测，静音回落后立即终止 `requestAnimationFrame` 循环（降至 **0 FPS**）；
  - 在界面顶部提供 **“冷驻留托盘模式（Cold Standby）”** 与 **“信号暂停/静音休眠”** 实测开关，直观展示销毁界面释放内存与秒级无损唤醒。

---

## 四、验收标准

1. **代码与结构完整性**：Rust 工作区（`fluideq-dsp-sys`、`fluideq-state`、`fluideq-shell`）结构完整，准确对接 `/native/dsp-core/include/fluideq/*.h`。
2. **自动化测试通过**：核心状态管理、冷驻留生命周期、滤波器与 APO 解析单元测试全部通过。
3. **运行态验证**：
   - 双层画布分离生效（静态曲线层与动态频谱层独立渲染）；
   - 静音或切换至冷驻留托盘模式时，频谱刷新率降至 `0 FPS`；
   - 从冷驻留托盘唤醒后，所有自定义的 EQ 频段、Tone 旋钮与 DSP 机架状态 100% 原样保留。

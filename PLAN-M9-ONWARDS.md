# FluidEQ 2.1 — M9 及后续阶段（M9–M14）深度演进与系统级闭环规划

> **背景说明**：随着第一阶段 **M1–M8**（C++ `dsp-core` 11 级机架 FFI 桥接、158+ 字二进制协议、16,384 点动静态线性相位 FIR 合成、冷驻留内存治理、8 层 `Include:` APO 配置树、8 槽位 `FxProperties` 注册表挂载规划、双卡槽无缝传输与 11 个原生守护进程协议）整体完成度推进至 **99.8%–100%**，FluidEQ 的核心算法与协议栈已在 Rust 侧全面落位。
>
> 本规划文档专门定义 **M9 至 M14（第二阶段：驱动级疑难声卡自愈、多节点网络音频重采样、压缩音频曲库扫描、Windows 权限/服务事务治理与 Tauri/WRY 原生宿主发布）** 的完整路线图、技术架构、验收指标与实时完成度。

---

## 一、M9–M14 模块总览与当前去水分真实完成度（截至更新 #11）

| 阶段编号 | 模块名称 | 对应源仓库 C++ / 系统子模块 | Rust 目标落点 | 纯算法/协议完成度 | 去水分真实总完成度 | 状态 |
| :--- | :--- | :--- | :--- | :---: | :---: | :---: |
| **M9** | **Windows APO 处理模式自愈、槽位记忆、Equalizer APO 剥离/还原与写一次备份** | `/native/system-apo/setup/fx_modes.cpp`<br>`slot_memory.cpp` / `fx_list.cpp` / `slot_report.cpp` / `backup.cpp` | `rust/fluideq-state/src/apo.rs` | **100%** | **88%** *(+33%)* | 纯逻辑已 100%，差 12% Win32 注册表落盘 |
| **M10** | **远程/局域网音频多相 Sinc 环形重采样、PI 时钟伺服、5ms 分包队列与镜像控制** | `/native/remote-audio-playback/src/sinc_kernel.cpp`<br>`playback_core.cpp` / `capture_queue.cpp` / `mirror_control.cpp` | `rust/fluideq-state/src/remote_playback.rs` | **100%** | **90%** *(+45%)* | 纯引擎已 100%，差 10% WASAPI 音频线程 |
| **M11** | **多线程无损/有损压缩音频解码管线与离线曲库响度/底噪扫描器** | `/native/dsp-host/src/decoders/compressed_decoder.cpp`<br>`/native/dsp-host/src/decoders/media_decoder.h` | `rust/fluideq-state/src/compressed_audio.rs`（规划） | **20%** | **15%** | ⏳ 待推进 |
| **M12** | **Windows 音频服务依赖图重启、注册表 ACL 权限交接与引擎 DLL SHA-256 摘要校验** | `/native/system-apo/setup/acl.cpp`<br>`/native/system-apo/setup/service_restart.cpp`<br>`/native/system-apo/setup/digest.cpp` | `rust/fluideq-state/src/apo_setup_tx.rs`（规划） | **15%** | **10%** | ⏳ 待推进 |
| **M13** | **Tauri 2.0 / WRY 原生无边框窗口、托盘冷驻留生命周期与全局硬件热键闭环** | 原 Electron 主进程窗口管理、托盘菜单与快捷键模块 | `rust/fluideq-shell/src/main.rs` | **65%** | **35%** | ⏳ 待本地宿主联调 |
| **M14** | **端到端硬件回归矩阵、MSVC 静态链接 `fluideq_dsp_core.lib` 与签名安装包发布** | CMake + Cargo 联合构建流水线与安装包脚本 | `/Cargo.toml` & `build.rs` | **50%** | **40%** | ⏳ 待本地编译环境验证 |

---

## 二、M9–M14 各模块详细架构设计与验收标准

### M9 — Windows APO 处理模式自愈与端点槽位记忆账本（已完成 100% ✅）
1. **核心痛点与设计目标**：
   - **声卡多模式旁路问题（如 Creative Sound BlasterX G6）**：部分声卡驱动在 `SFX` 或 `MFX` 槽位声明了专属的电影/媒体处理模式（Processing Modes，`{d3993a3f-99c2-4402-b5ec-a92a0367664b},5/6/7`）。若 APO 注册时仅写入 `DEFAULT` 模式，Windows 会将标记为电影或媒体的音频流路由至厂商模式从而完全绕过 FluidEQ；反之，若在单值槽位漏写处理模式属性，Windows 只会将 APO 列为“仅发现（Discovery-only）”而从不实例化。
   - **驱动静音槽位记忆与阶梯回退（Slot Memory Ladder）**：部分蓝牙耳机或特殊驱动在默认的 `efx` 槽位挂载后看似成功但从不回调音频流。需要为每个端点（`<engine root>\slots\<guid>.txt`）按时间顺序记录已尝试过的槽位，既避免重复落入已证实无声的槽位，又能在未来阶梯中间新增槽位时让老设备自动探测到新槽位。
2. **Rust 实现落点（`rust/fluideq-state/src/apo.rs`）**：
   - `modes_for_empty_slot`：`EFX` 仅写入 `DEFAULT`；`SFX` / `MFX` 写入 `DEFAULT` 并自动复制兄弟流槽位中除 `RAW`（`{9E90EA20-B493-4FD1-A1A8-7E1361A956CF}`）以外的所有厂商模式；
   - `mode_missing` 与 `plan_mode_in_place`：对旧版安装助手遗留的缺失处理模式端点执行原地无损补齐，不改动槽位与厂商效果器；
   - `ApoSlot::slot_name`、`parse_remembered_slots`、`append_remembered_slot`、`first_untried_ladder_slot` 与 38 字符严格大括号 GUID 校验器（`is_valid_endpoint_guid`，防止提权进程路径穿越）。

---

### M10 — 远程/局域网音频多相 Kaiser-Bessel Sinc 重采样、环绕声折混与自适应抗抖动缓冲（已完成 100% ✅）
1. **核心痛点与设计目标**：
   - **跨设备时钟漂移与采样率转换**：局域网发送端（44.1 / 48 / 96 kHz）与接收端声卡存在时钟微差与采样率差异，需要零锁、零动态分配的高精度多相重采样器。
   - **多声道环绕声与立体声自动折混（Fold-down）**：5.1（掩码 `0x3f`）与 7.1（掩码 `0x63f`）源流发往立体声或不同环绕布局声卡时，需按标准声学权重（中置 $-3\text{ dB} = 0.70710678$、低音炮 $0.25$、侧环绕/后环绕 $0.5$）构建 $8 \times 8$ 路由矩阵。
   - **低延迟模式 vs 音乐平滑模式抗抖动缓冲（Adaptive Jitter Cushion）**：普通模式从 `30 ms` 起步（下限 `15 ms`，上限 `160 ms`，每次断流步进 `+10 ms`）；音乐平滑模式从 `300 ms` 起步（上限 `2000 ms`），并在遇到 Wi-Fi 突发卡顿后自动测量停顿帧数（`starve_frames`），在原有水位上叠加 `150 ms + 停顿时长` 进行一次性学习，连续稳定播放 60 秒后每分钟回落 `1 ms`。
2. **Rust 实现落点（`rust/fluideq-state/src/remote_playback.rs`）**：
   - `bessel_i0` 与 `build_sinc_kernel`：生成 $(1024 + 1) \times 64 = 65,600$ 点的 Kaiser-Bessel（$\beta = 10$）窗多相 Sinc 插值表，每相独立归一化确保直流增益严格为 `1.0`；
   - `build_speaker_folddown_matrix`：完整实现 $1 \sim 8$ 声道间的直通、单声道复制、5.1/7.1 立体声折混及侧/后环绕声道等价映射；
   - `AdaptivePlaybackCushion`：完整实现双档位抗抖动缓冲水位学习与 60 秒稳定衰减状态机。

---

### M11 — 多线程无损/有损压缩音频解码管线与离线曲库响度/底噪扫描器（下一阶段重点）
1. **架构目标**：
   - 在已完成的 `wav_ir.rs`（RIFF/WAVE + FORM/AIFF/AIFC 全格式解码）基础上，规划纯 Rust 压缩音频解码与曲库分析器（`compressed_audio.rs`）：
     - 按内容魔数（而非文件扩展名）识别 FLAC（`fLaC`）、Ogg Vorbis（`OggS`）、MP3（`ID3` 或帧同步头）与 AAC/M4A（`ftyp`），严格对齐 `pcm_decoder.cpp` 中“先匹配严格 PCM 魔数、再匹配压缩格式、最后回退系统解码器”的防误判顺序；
     - 提供离线曲库扫描算法：在后台计算曲目的双增益（峰值与响度增益 `set_track_gains`）以及 31 字 `FeqNoiseProfile`（16 段本底噪声 + 50/60 Hz 交流哼声基频与 6 阶谐波余量）。
2. **验收标准**：
   - 魔数嗅探器对伪装为 `.wav` 的 MP3 载荷及重命名音频文件实现 100% 准确分流；
   - 曲库扫描器输出的 31 字底噪向量可直接喂入 `AppStateStore::apply_noise_profile_wire`。

---

### M12 — Windows 音频服务依赖图重启、注册表 ACL 权限交接与引擎 DLL SHA-256 摘要校验
1. **架构目标**：
   - 移植 `/native/system-apo/setup/digest.cpp`、`service_restart.cpp`、`acl.cpp` 与 `attach_outcome.cpp` 中的纯逻辑与事务规划器：
     - **SHA-256 完整性校验与升级判定**：计算已安装 `FluidEQEngine.dll` 与安装包内 DLL 的 SHA-256 摘要，决定是否需要采用延迟重命名（Pending File Rename）或热替换策略；
     - **`FxProperties` 注册表 ACL 权限模型**：由于 Windows 默认将音频端点的 `FxProperties` 子键所有者设为 `NT SERVICE\TrustedInstaller`，规划安全的权限获取（Take Ownership -> 授予 Administrators 写权限 -> 写入 APO 槽位 -> 还原安全描述符）状态机；
     - **音频服务拓扑重启序列**：按依赖顺序停止 `Audiosrv` 及其依赖服务、重启 `AudioEndpointBuilder` 并拉起 `Audiosrv`，附带指数退避重试机制（`retry.cpp`）。
2. **验收标准**：
   - 纯 Rust SHA-256 摘要比对、服务重启状态序列与挂载结果分类（`attach_outcome`：成功、需重启音频服务、被第三方独占拒绝）通过全部固定夹具单元测试。

---

### M13 — Tauri 2.0 / WRY 原生无边框窗口、托盘冷驻留生命周期与全局硬件热键闭环
1. **架构目标**：
   - 在本地 Windows 宿主机上，将 `fluideq-shell` 的 `dispatch_ipc_command` 与 `SpectrumBridge` 绑定至 Tauri 2.0 / WRY WebView；
   - 当用户点击最小化或关闭至系统托盘时：
     1. 立即进入 `MinimizedWarm`（停止 60 FPS 频谱 IPC 推送与 C++ FFT 分析）；
     2. 连续 15 秒无操作且无在线媒体播放锁定时，自动调用 `webview.destroy()` 释放 Chromium 渲染进程（转入 `ColdStandby`，内存降至 `< 12 MB`）；
     3. 点击托盘图标唤醒时，在 `< 50 ms` 内重建窗口并通过 `GetSnapshot` 一次性恢复全部 UI 状态。

---

### M14 — 端到端硬件回归矩阵、MSVC 静态链接 `fluideq_dsp_core.lib` 与签名安装包发布
1. **架构目标**：
   - 在 Windows + MSVC + CMake + Rust 工具链环境下开启 `--features link-cpp-core`，通过 `fluideq-dsp-sys/build.rs` 将 `/native/dsp-core` 编译为静态库并开启跨语言 LTO（Link-Time Optimization）；
   - 运行涵盖全部 11 个子模块的 `cargo test --workspace` 回归套件，并打包生成单文件免依赖安装包。

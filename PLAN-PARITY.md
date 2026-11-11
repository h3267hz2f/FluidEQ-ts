# Rust 桌面版后续计划（对齐上游 `StartSWest/FluidEQ`）

> 更新日期：2026-11-11
> 基线分支：`dsv4.1f`（EFX 单槽链路与输出跟随已在实机抽查通过；Settings / Config / Games / DSP rack 开关与 24 参数已合入）
> 对照基准：上游完整仓库 [`StartSWest/FluidEQ`](https://github.com/StartSWest/FluidEQ)（而非本仓库裁剪过的 5 组件预览 `src/`）

## 一、现状基线（对照上游 `StartSWest/FluidEQ`）

### 1.1 各层真实对齐度

| 维度 | 真实进度 | 现状说明 |
| --- | --- | --- |
| **C++ 原生层 (`native/`)** | **~100%（共用）** | 12 个原生子工程（`dsp-core`、`dsp-host`、`system-apo`、5 个 watcher、`lighting-host`、`wallpaper`、`remote-audio-capture/playback`）直接沿用上游源码；当前安装包已打入 `Engine.dll`、`Engine-Setup.exe`、`DSP.exe`、`Outputs.exe`、`Games.exe` |
| **Rust 状态与协议层 (`fluideq-state` + `system_daemons.rs`)** | **~65%** | 11 级链二进制协议、RBJ/Vicanek 滤波器、APO 槽位阶梯、OPRA 分片读取、KEMAR 人头解析、WAV/压缩容器嗅探、LAN 重采样队列、各 watcher 协议解析已移植并通过单测（state 105 项、shell 58 项）；但部分模块仍停留在纯内存结构体，**未接真实 I/O 或守护进程** |
| **Tauri 宿主生命周期** | **~85%** | 托盘、单实例、开机启动、全局热键、窗口位置持久化、默认输出跟随、前台游戏预设接管与还原均已落地；仅剩 `ColdStandby` 销毁 webview |
| **Tauri 桌面 UI (`fluideq-shell/ui`)** | **~15–20%** | 已完成 15 段 EQ、预设、卷积加载、Config 只读诊断、Games 映射、DSP rack 11 级开关+24 参数、Settings；上游大量核心调音控件与外围工作区尚未建页或接线 |
| **打包发布** | **~55%** | NSIS per-user 可出包，CI 校验引擎文件与 `SHA256SUMS.txt`；缺代码签名、升级/卸载回滚验证、完整真机矩阵 |
| **综合评估（以核心系统调音台为分母）** | **~50–55%** | EQ + 耳机纠偏 + 卷积 + DSP 机架 + Room + 设备/游戏跟随 |
| **综合评估（以上游完整产品为分母）** | **~25–30%** | 含曲库播放器、卡拉 OK & AI 分轨、3D 可视化/壁纸/灯效、LAN 共享、12 语言、F1 指南 |

### 1.2 上游功能面逐项对照

| 上游模块 | 上游 `StartSWest/FluidEQ` 实际能力 | 当前 Rust Tauri 版状态 |
| --- | --- | --- |
| **EQ 核心** | 最多 128 频段、7 种滤波器（PK/LSC/HSC/LP/HP/BP/Notch）、左右耳独立频段、Bass/Mid/Treble Tone 旋钮 + 高低切、图形直接标数值 | 固定 15 频段（PK/LSC/HSC）、无左右耳独立 UI、**Tone 与高低切后端有字段但无 UI 旋钮** |
| **7 层独立配置链** | Convolution / Driver type(12种) / Headphone(OPRA) / Parametric EQ / Tone / Preset(107个) / Smart EQ 各自独立 `.txt` include、独立开关与 0–100% 强度、`.fluideq` 导入导出 | 仅单一主曲线 + 用户预设 + 本地 WAV/AIFF 卷积；**无 7 层分层开关/强度、无驱动类型补偿、无 `.fluideq` 导入导出** |
| **耳机库 & 测量** | 离线 OPRA（6,229 型号 / 12,594 曲线）搜索选择器、Squiglink 导入向导、My curves、AutoEq 脉冲在线检索下载、Smart EQ / Continuous EQ 实时测量拉平 | `opra.rs` 后端可解曲线，但 **UI 无 OPRA 选择器、无 Squiglink 导入、无 Smart EQ** |
| **DSP 机架** | 11 级 + 曲间 Crossfade；每级带实时曲线/衰减图、Isolate（只听差值）、Gain match、阶段预设、Exciter 三频段放置、Denoise 神经网络人声清理 | 已有 11 级开关 + 24 个主控滑杆；**无阶段内实时电表/图形、无 Isolate/Gain match 按钮、无阶段预设** |
| **Room（房间）** | 24 个测量房间预设、俯视 7.1 音箱摆位拖拽（角度/距离/电平/Mute/Solo）、低音管理、5 对盲听测头型 | `room_heads.rs` 已能解析 KEMAR，但 **Room 导航按钮仍为 disabled**（仅机架内有 3 个基础滑杆） |
| **设备与第二输出** | 按端点记忆全套档案；**Second output** 同时镜像到多台设备并各自套用独立档案 | 默认输出跟随与档案记忆已通；**Second output 镜像未接线** |
| **Gaming Lab** | 可折叠分栏编辑器、进程黑名单（排除系统/启动器）、桌面切换通知、每游戏独立档案副本 | 已有 `games.json`（exe → 预设名）+ `FluidEQ-Games.exe` 接管与还原；无运行中进程选择器与系统进程黑名单 |
| **播放器 / Library / 紧凑模式** | 文件夹增量扫描、内嵌封面、Album/Artist/Genre/Folder/Video 视图、Cover Flow、每首歌记住调音、底部全局 Transport 栏（接管系统 SMTC）、Compact Player 小窗模式 | **0%（未建 UI，未接 `media-watch` / `volume-watch` 常驻）** |
| **Karaoke & Maker** | `.lrc` / UltraStar 舞台、麦克风实时音高检测与打分；Maker 含本地 GPU 人声分离、Whisper 语音转写、旋律提取 | **0%** |
| **Share Audio** | 局域网双向加密无损音频共享（连接码配对、`remote-audio-capture/playback`） | 协议算法在 `remote_playback.rs`，**守护进程未打包、UI 未建** |
| **Online Media** | 内嵌独立 Cookie 仓的受限流媒体视图（YouTube / Bandcamp / Twitch / Suno）边播边调 | **0%** |
| **Visualizers / Studio / 壁纸 / 灯效** | 40 种 GPU 频谱外观、3D 场景、AI Studio、多屏动态壁纸（`FluidEQ-Wallpaper`）、Windows Dynamic Lighting & Razer Chroma（`lighting-host`） | **0%（仅 `system_daemons.rs` 有协议结构体，未拉起进程也无画布）** |
| **国际化 / 指南 / 更新** | 12 语言切换、F1 模糊搜索图文手册、后台自动更新与签名校验 | 仅英文硬编码、无 F1 手册、无自动更新 |

---

## 二、目标与重排后的优先级原则

1. **先补齐核心调音台（Core Audio Console First）**：先把系统级调音台剩下的 45%（Tone/高低切、OPRA 耳机选择与 Squiglink 导入、Room 独立页、实时频谱/电表接入、机架 Isolate）做实，让调音核心达到 100% 可用，再碰曲库播放器、卡拉 OK 或 3D 场景。
2. **单一事实来源**：参数表、预设、设备配置全部收敛到 `fluideq-state`，UI 只做展示与命令转发。
3. **复用已移植后端**：`opra.rs`、`room_heads.rs`、`spectrum_bridge.rs`、`remote_playback.rs`、`system_daemons.rs` 已有算法与单测，优先接线暴露命令与 UI，不重写。
4. **可退役标准**：本仓库 `src/` 仅为裁剪版预览桥；待核心调音台与参数表唯一来源落地后即可删除预览桥。
5. **每阶段可独立出包验证**：每次推送跑通 `cargo test` + `clippy -D warnings`，关键节点触发 `rust-desktop.yml` 出包上真机。

---

## 三、阶段计划

### P0 — 系统链路收尾与防回归（已完成主体，剩真机矩阵）

- [x] 槽位决策回归矩阵，Rust 与 C++ 双端同用例（`apo.rs`、`fx_list.cpp`、`fx_list_test.cpp`）。
- [x] 阶梯记忆接线到实际 attach 流程：失败槽位写入 `<engine root>\slots\<guid>.txt`，下次自动跳过。
- [x] 加载失败诊断闭环：`status-<GUID>.json` 的 `loaded` 为 false 时，UI 明确显示原因与下一步；解析 `engine.log` 关键行。
- [x] preamp 语义收口：manual 默认 0，总增益低于 −30 dB 告警，补纯函数单测。
- [ ] 真机复测：Realtek 共享模式、USB DAC、蓝牙各一次，记录端点 GUID 与最终槽位。（2026-11-11 已抽查部分场景，用户确认无问题；USB DAC、蓝牙、游戏监听与机架开关待实机补测）

### P1 — 基础页面与宿主闭环（已完成）

- [x] UI 按页面拆文件（`ui/views/*.js` + `registry.js`），导航按 `data-view` 切换。
- [x] EQ 预设（`list_presets` / `apply_preset` / `save_preset` / `delete_preset`）。
- [x] 卷积加载与清除（`load_ir` / `clear_ir` + IR 摘要展示）。
- [x] Config 只读页（端点槽位/挂载/加载状态、已存端点档案、当前曲线与 preamp、引擎裁决日志）。
- [x] Games 页与前台监听（`games.json`、`FluidEQ-Games.exe` 打包、`hold <pid>` 跨 alt-tab、退出还原曲线与预设名）。
- [x] DSP rack 第一、二版（11 级真实开关 `set_stage_enabled` + 24 个主控参数 `set_stage_parameter` 滑杆与后端钳制）。
- [x] Settings 页与宿主生命周期（托盘图标/菜单、关闭到托盘持久化、单实例、开机启动、全局热键冲突提示、窗口位置多屏防越界、后台默认输出跟随 `FluidEQ-Outputs.exe`）。

### P2 — 核心调音台补齐（当前主攻，补齐核心 45%）

按投入产出比排序，每项均复用 `fluideq-state` 已有模块：

| 顺序 | 模块 | 具体交付内容 | 复用后端 |
| --- | --- | --- | --- |
| 1 | **Tone 与高低切控件** | [x] 已完成：EQ 页补齐 Bass / Treble 搁架、Low cut / High cut 频率控制、EQ 模型（Clean / Proportional Q / Wide / Asymmetric）、Character 强度与 Matched treble 开关，同步到 APO 配置与实时曲线 | `ToneSettings`、`set_tone_and_voicing` |
| 2 | **OPRA 耳机库选择器** | [x] 已完成：暴露品牌与型号检索命令（`list_opra_vendors` / `list_opra_models` / `apply_opra_curve`），在 EQ 检查器中提供离线耳机库级联选择与一键曲线校准应用 | `opra.rs`、`apply_opra_headphone_curve`、`/opra/curves` |
| 3 | **Room 独立页（启用导航）** | [x] 已完成：解锁导航栏 Room 按钮，新增 Room 独立视图，支持 5 种声学空间预设（Studio / Stage / Cinema / Arena / Club）、3 档 KEMAR 头型脉冲切换（Small / Medium / Large）、空间几何尺寸与混响衰减微调、低音管理与声道电平控制 | `room_heads.rs`、`load_room_head_from_text`、`FeqChainRoomSettings` |
| 4 | **频谱与多级电表接入** | 将 `spectrum_bridge.rs` 接入 Tauri 事件循环，在 EQ 曲线底部绘制实时频谱，遵守 `MinimizedWarm` 暂停推送约定 | `spectrum_bridge.rs`、`residency.rs` |
| 5 | **DSP 机架进阶控制** | 补齐各阶段 `isolate`（只听差值）开关、Normalizer 模式切换（Off / Guard / Leveler）、Master 交付目标预设（Streaming -14 LUFS / Cinema / Reference 等） | `FeqChainSettings`、`persistence.rs`（启动自动清零 `isolate`） |
| 6 | **更多滤波器类型与频段开关** | 单频段启用/旁路开关、滤波器类型切换（PK / LSC / HSC / LP / HP / BP / Notch）、左右耳（Both / Left / Right）选择 | `biquad.rs` (`BandFilterKind`, `ear`) |
| 7 | **Squiglink / Equalizer APO 文本导入导出** | 支持导入/导出标准 Equalizer APO `Filter:` / `Preamp:` 文本，直接生成或覆盖频段 | `apo.rs` |

### P3 — 发布加固、冷驻留与预览桥退役

- [x] **M11 压缩音频嗅探**：`compressed_audio.rs` / `wav_ir.rs` 魔数分流已就位。
- [x] **M12 系统事务**：SHA-256 摘要、服务依赖图重启、注册表 ACL 交接单测已就位。
- [ ] **冷驻留收口**：确认 `MinimizedWarm` 停频谱推送闭环；`ColdStandby` 视唤醒体验作为可选策略。
- [ ] **参数表唯一来源与预览桥清理**：把 `native/tools/generate-parameters.mjs` 的参数源头收敛，清理本仓库裁剪版 `src/` 与 `server.ts`。
- [ ] **M14 发布与真机矩阵**：安装包代码签名、升级/卸载回滚验证、补全 Realtek / USB DAC / 蓝牙真机回归记录。

### P4 — 外围大型工作区（远期按需推进）

这些模块在上游占很大代码体积，但独立于系统级 EQ 核心，按需分阶段立项：

1. **系统媒体栏与小窗（Transport & Compact Player）**：接入 `FluidEQ-Media`（SMTC）与 `FluidEQ-Volume`。
2. **第二输出与局域网共享（Second Output & Share Audio）**：打包 `remote-audio-capture/playback`，建配对与路由页。
3. **本地曲库播放器（Library）**：文件夹异步扫描、标签/封面解析、双 Deck 播放与曲间 Crossfade。
4. **视觉与外设（Visualizers / Wallpaper / Lighting）**：打包 `FluidEQ-Wallpaper` 与 `lighting-host`，接 GPU 频谱外观与灯效。
5. **多语言与内置手册（i18n & F1 Guide）**：12 语言词条表与离线手册检索。
6. **卡拉 OK 与 AI 分轨（Karaoke & Maker）**：`.lrc`/UltraStar 渲染与按需下载 ONNX 模型。

---

## 四、测试与 CI 门禁

- 每次推送：`cargo test --workspace`、`cargo clippy --workspace -- -D warnings`、原生 `ctest`（`fx_list_test` 等）。
- UI 改动：本地用静态服务 + `__TAURI__` mock 跑通空态与正常态 DOM 断言，无控制台报错再提交。
- 打包：`rust-desktop.yml` 手动 `bundle=true`，在阶段节点触发，校验引擎二进制与 `SHA256SUMS.txt`。

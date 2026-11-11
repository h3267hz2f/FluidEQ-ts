# Rust 桌面版后续计划（对齐 Electron）

> 基线日期：2026-11-10
> 基线版本：`dsv4.1f`（EFX 单槽链路已在 Realtek 端点实测出声并生效）

## 一、现状基线

| 维度 | 现状 | 完成度 |
| --- | --- | --- |
| DSP / APO / 协议核心（M1–M10） | 纯逻辑已移植；注册表落盘、WASAPI 音频线程仍部分依赖 C++ helper | 85–90% |
| 桌面 UI 与用户功能 | 只有 EQ 频段页；DSP rack / Room / Settings 为 disabled 占位；无预设、卷积、曲库、卡拉 OK、共享音频、在线媒体 | 20–25% |
| 宿主生命周期 | 无托盘、无单实例、无开机启动、无全局热键、无真正冷驻留 | 10–15% |
| 打包发布 | NSIS per-user 可出包；无签名、无真机回归矩阵 | 50% |
| **加权整体（后端 60% / 前端 40%）** | | **约 55%** |

结论：**系统级 EQ 的核心链路已经打通，下一阶段的瓶颈不再是算法，而是 UI 对齐与宿主完整性。** 计划按此优先级推进。

## 二、目标与原则

1. **先能用，再完整**：保持系统 EQ 在真实声卡上的稳定性优先于新页面。
2. **单一事实来源**：参数表、预设、设备配置全部收敛到 `fluideq-state`，UI 只做展示与命令转发。
3. **不重复实现**：Rust 侧已移植的能力（`opra.rs`、`wav_ir.rs`、`device_profiles.rs`、`remote_playback.rs`、`residency.rs`、`system_daemons.rs`）优先接线，不重写。
4. **可退役标准**：只有 Electron `src/` 的全部功能在 Tauri 侧可用，才删除 `src/` 与 `server.ts` 预览桥。
5. **每阶段可独立出包验证**：不把三个月的改动压成一次发布。

## 三、阶段计划

### P0 — 系统链路收尾与防回归（3–5 天）

**目标**：任何一台 Windows 机器上，助手都能选到能真正加载的槽位，且失败原因可见。

- [x] 槽位决策回归矩阵，Rust 与 C++ 双端同用例：
  - composite 列表 + EFX 被 Equalizer APO 占用 → `EfxSingle`
  - composite 列表 + EFX 被 Windows 默认 APO 占用 → `EfxSingle`
  - EFX / MFX / SFX 单值都被厂商占用 → 阶梯回退到 composite，不越权写入
  - 纯 legacy 端点、裸端点、Windows 两代默认 APO 四种 GUID 拼写
  - 文件：`rust/fluideq-state/src/apo.rs`、`native/system-apo/setup/fx_list.cpp`、`native/system-apo/tests/fx_list_test.cpp`
- [x] 阶梯记忆接线到实际 attach 流程：失败槽位写入 `<engine root>\slots\<guid>.txt`，下次自动跳过；验证 `first_untried_ladder_slot` 被调用而不是只存在于测试。
- [x] 加载失败诊断闭环：`status-<GUID>.json` 的 `loaded` 为 false 时，UI 明确显示“已写入但未实例化 / 未重启音频服务 / 运行时缺失”；补 `engine.log` 关键行解析。
- [x] preamp 语义收口：
  - 保持“auto headroom + manual trim 相加”的物理事实，但 UI 默认 manual 为 0，避免再次出现 −38 + −24 = −62 dB 的静音错觉；
  - 总增益低于 −30 dB 时给出提示（曲线峰值过高，建议降增益），而不是无声；
  - 为叠加逻辑补一个纯函数测试。
- [ ] 真机复测：Realtek 共享模式、USB DAC、蓝牙各一次，记录端点 GUID 与最终槽位。（2026-11-11 已抽查部分场景，用户确认无问题；剩余端点待补）

**退出标准**：三台/三类端点均能自动选槽并加载；失败时 UI 能指出具体原因。

### P1 — Electron UI 对齐（3–4 周）

**目标**：主界面功能面与 Electron 一致，用户无需回退。

优先顺序按使用频率排列：

| 顺序 | 页面 | 需要补的 Rust 命令 | 复用后端 |
| --- | --- | --- | --- |
| 1 | EQ 预设（`eq-presets`） | `list_presets` / `apply_preset` / `save_preset` / `delete_preset` | `models.rs`、持久化 |
| 2 | 配置（`config`）✅ 只读页 | 无新增命令，复用 `engine_status` 与快照：端点槽位/挂载/加载状态、已存端点档案、当前曲线与 preamp、引擎自身日志 | `device_profiles.rs`、`engine_status` |
| 3 | 卷积（`convolution`）✅ | `load_ir` / `clear_ir` 已接，IR 摘要随快照下发 | `wav_ir.rs`、`linear_phase.rs` |
| 4 | 游戏预设（`games`）✅ | `list_game_presets` / `set_game_preset` / `remove_game_preset` / `game_watch_available`；`FluidEQ-Games.exe` 监听前台、`hold` 跨 alt-tab、退出还原 | `game_presets.rs`、`foreground-watch` |
| 5 | DSP rack ✅ | `set_stage_enabled`（11 级开关）+ `set_stage_parameter`（24 个主控参数，后端钳制）；每级 1–4 条滑杆，余下细参数待补 | `rack_stages.rs`、链设置快照 |
| 6 | Settings ⏳ | 启动项/关闭到托盘/热键状态已接；语言、主题、更新检查待补 | 现有 shell 命令 |
| 7 | Room | 房间校正参数与测量入口 | `room_heads.rs` |
| 8 | Library / Karaoke / Share audio / Online media / Plus | 见 P3 | `remote_playback.rs`、`system_daemons.rs` |

实现要求：

- [x] UI 继续用原生 `index.html` + `app.js`，但按页面拆文件（`ui/views/*.js`），避免 `app.js` 继续膨胀。频段页、Settings 页已拆分，导航按 `data-view` 切换。
- [x] 每个命令在 `fluideq-shell/src/main.rs` 注册，失败返回结构化错误而不是裸字符串。
- [ ] 复用现有 `spectrum_bridge.rs`，页面切换时遵守冷驻留的推送暂停约定。
- [ ] 每页完成即补三条冒烟检查：空状态、正常值、越界值。

**退出标准**：Electron 的 7 个主标签页、5 个 EQ 子页在 Tauri 侧全部可用；截图对照无功能缺口。

### P2 — Tauri 宿主生命周期（1–2 周）

- [x] 托盘图标 + 菜单（显示/隐藏、旁路、退出），关闭窗口默认最小化到托盘（可在设置中改）。
- [x] 单实例：第二次启动唤起已有窗口，不重复占用 owner pipe。
- [x] 开机启动（per-user Run 项），与安装器选项一致。
- [x] 全局热键：旁路开关、音量增减，冲突时提示而不静默失败。
- [ ] 冷驻留真正落地：`residency.rs` 的状态机接入 Tauri 窗口事件，`MinimizedWarm` 停频谱推送，`ColdStandby` 销毁 webview；唤醒 50 ms 内恢复快照。
- [x] 窗口位置/大小/最大化状态持久化，多显示器下不越界。

**退出标准**：关闭到托盘后内存占用与 Electron 版对比不劣化；冷驻留唤醒可重复 100 次无泄漏。

### P3 — 缺失后端能力（2–3 周）

- [ ] **M11 压缩音频**：新增 `rust/fluideq-state/src/compressed_audio.rs`，按魔数分流 FLAC / Ogg / MP3 / AAC，对齐 `pcm_decoder.cpp` 的判定顺序；曲库离线响度与底噪扫描。
- [x] **M12 系统事务**：SHA-256 摘要、服务依赖图重启、注册表 ACL 交接从 C++ helper 迁到 Rust 或明确保留 helper 边界；先补纯逻辑单测，再上真机。
- [ ] **M14 发布**：签名安装包、升级/卸载回滚验证、CI 产出哈希清单。
- [ ] 回归矩阵：Windows 10/11、Realtek / USB DAC / 蓝牙、共享模式 / 独占模式、per-user 安装。

**退出标准**：不依赖 Electron 也能完成曲库导入、设备切换、升级安装。

### P4 — Electron 退役（3–5 天）

- [ ] 把 `src/common/dsp/nativeParameters.ts` 的参数表迁入 Rust 作为唯一来源，删除 TypeScript 副本。
- [ ] 删除 `src/`、`server.ts` 预览桥、Vite/Electron 相关配置与 CI 工作流。
- [ ] 更新 `README.md`、`CLAUDE.md`、`PROGRESS.md`，移除“预览服务端”路径。
- [ ] 打一个 `2.2.0` 标签，保留旧版安装包一个版本周期。

**退出标准**：仓库中不再存在 Electron 构建路径；`cargo test --workspace` 与原生测试全绿。

## 四、里程碑时间表

| 周 | 内容 | 可交付物 |
| --- | --- | --- |
| W1 | P0 全部 | 槽位回归测试 + 失败诊断 + preamp 默认值修正 |
| W2–W3 | P1 第 1–4 项 | EQ 预设、配置、卷积、游戏预设页 |
| W4–W5 | P1 第 5–8 项 | DSP rack、Settings、Room、其余页面骨架 |
| W6 | P2 | 托盘、单实例、热键、冷驻留 |
| W7–W8 | P3 前两项 | 压缩音频、系统事务 |
| W9 | P3 发布项 + 回归 | 签名包、真机矩阵报告 |
| W10 | P4 | 删除 Electron 路径，2.2.0 |

单人节奏估算；若只能投入半天/天，按两倍时间排。

## 五、测试与 CI 门禁

- 每次推送：`cargo test --workspace`、`cargo clippy --workspace -- -D warnings`、原生 `ctest`（57 套）、`fx_list_test`。
- UI 改动：本地起 `cargo tauri dev` 走三条冒烟路径（加载、改增益、旁路），不通过不合并。
- 打包：`rust-desktop.yml` 手动 `bundle=true`，只在 P0/P1/P2 阶段末触发，避免每次推送都出包。
- 真机记录：每个阶段结束更新一份端点矩阵（GUID、槽位、`loaded`、延迟）。

## 六、风险与对策

| 风险 | 影响 | 对策 |
| --- | --- | --- |
| 不同声卡只认某些槽位 | 用户“没效果” | 阶梯记忆 + 失败诊断（P0） |
| Equalizer APO 共存 | 抢占或双 EQ 叠加 | 保留现有“仅 EAPO 可让位”规则，附 detach 还原测试 |
| preamp 叠加导致静音 | 误判为开关故障 | manual 默认 0、总增益提示、下限保护（P0） |
| UI 页面多、单文件膨胀 | 维护成本高 | 按页拆文件，命令层统一错误结构（P1） |
| 冷驻留销毁 webview 引入卡顿 | 唤醒体验差 | 先做 `MinimizedWarm`，`ColdStandby` 作为可选开关灰度 |
| 压缩解码格式兼容 | 曲库导入失败 | 魔数优先、系统解码器兜底、固定夹具测试 |

## 七、本周可立即执行的 10 个任务

1. 给 `default_slot_for` 的四种 Windows GUID 拼写补 Rust 参数化测试。
2. 确认 `first_untried_ladder_slot` 已接线；若未接线，在 attach 失败路径写入阶梯记忆。
3. `status-<GUID>.json` 的 `loaded=false` 在 UI 上显示具体下一步（重启音频服务 / 检查运行时 / 换槽位）。
4. 把 manual preamp 默认值改为 0，并补叠加总增益的单元测试。
5. 总增益低于 −30 dB 时在 preamp 读数旁给出警告文案。
6. 新建 `ui/views/`，把频段页从 `app.js` 拆出，建立视图注册表。
7. 实现 `list_presets` / `apply_preset` 两个命令与预设下拉框。
8. 实现 `load_ir` / `clear_ir` 与卷积开关。
9. 加托盘图标与“关闭到托盘”最小实现，验证 owner pipe 不被重复启动占用。
10. 在 CI 增加 `cargo clippy -D warnings` 门禁，避免 UI 接线阶段引入回归。

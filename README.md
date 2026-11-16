**简体中文** · [繁體中文](README.zh-TW.md) · [English](README.en.md) · [日本語](README.ja.md)

# wivrn-pose-holder

[z3y/driver-pose-holder](https://github.com/z3y/driver-pose-holder) 的 Linux／WiVRn 版：手柄丢失追踪时，把它定在最后一个有效姿态，不让游戏把手收走。

原版是 SteamVR 驱动，挂在 `TrackedDevicePoseUpdated` 上。WiVRn + xrizer 的环境里没有 SteamVR，所以这里做成 **OpenXR API layer**，夹在应用程序（xrizer）和 runtime（WiVRn）中间。不用改 WiVRn，也不用改 xrizer，两边更新后不必重新编译。

```
VRChat → xrizer → [pose_holder] → WiVRn → Quest
```

## 做了什么

| 拦截的函数 | 行为 |
|---|---|
| `xrLocateSpace` | 只处理手柄的 action space。位置或方向无效时，返回最后一个有效姿态，速度归零。追踪恢复后的前 5 帧仍然用旧姿态（Quest 刚恢复时上报的姿态还是错的，手会瞬移一下） |
| `xrGetCurrentInteractionProfile` | 手柄休眠时 Quest 会上报“没有 interaction profile”，xrizer 看到就把手柄标成断开。这里继续上报上一个 profile，手柄保持连接 |
| `xrGetActionStatePose` | 同上，pose action 保持 active |
| `xrPollEvent` | 手柄休眠和唤醒时，WiVRn 都会发出“interaction profile 变了”的事件，xrizer 收到就重建所有手柄和追踪器（包括 Rebocap 的）。在应用程序看来 profile 没变时，这个事件会被拦掉；真的变了（例如切到手部追踪）才放行 |

头显、Rebocap 追踪器（xdev space）和按键输入都不经过这里，原样通过。从手柄切到手部追踪时，profile 会变成另一个有效值，也照常通过。

## 安装

从 Releases 下载 `wivrn-pose-holder-v*-x86_64.tar.gz`（已编译好，不需要 Rust），解压后执行：

```sh
./install.sh
```

或从源码安装（需要 Rust）：clone 这个 repo，在文件夹里执行同一个 `./install.sh`，脚本会自己编译。

脚本会放两个文件：

```
~/.local/share/wivrn-pose-holder/libxr_pose_holder.so
~/.local/share/openxr/1/api_layers/implicit.d/XrApiLayer_pose_holder.json
```

这是 implicit layer，这个用户开的所有 OpenXR 程序都会加载，Proton 里的 xrizer 也是（家目录在容器里看得到）。装完重开 VRChat 就生效。

卸载：`./install.sh --uninstall`

## 设置

都是环境变量。要对 VRChat 设置的话，加在 Steam 启动选项最前面。

| 变量 | 默认 | 说明 |
|---|---|---|
| `POSE_HOLDER_MIN_VALID_FRAMES` | `5` | 追踪恢复后，要连续这么多帧有效才采用新姿态。恢复时手还是会跳的话调大 |
| `POSE_HOLDER_MAX_HOLD_SECS` | `0` | 最多定住几秒，超过就放手。`0` 是一直定住（和原版一样） |
| `POSE_HOLDER_HOLD_CONNECTION` | `1` | 设成 `0` 就只定住姿态，不管手柄的连接状态 |
| `DISABLE_XR_APILAYER_POSE_HOLDER` | | 设成任何值就不加载这个 layer |

## 确认有没有加载

layer 会写到 stderr：

```
[pose_holder] active (min_valid_frames=5, max_hold=forever, hold_connection=true)
[pose_holder] tracking lost on action 0x… path 0x…, holding last pose
[pose_holder] tracking recovered on action 0x… path 0x…
```

VRChat 的 stderr 可以用启动选项 `PROTON_LOG=1 %command%` 存到 `~/steam-438100.log`。也可以用 `XR_LOADER_DEBUG=info` 跑任何 OpenXR 程序，看有没有 `succeeded loading layer XR_APILAYER_TW527E_pose_holder`。

## 测试

```sh
cargo test
```

`tests/mock_runtime.rs` 用一个假的 runtime，照 OpenXR loader 的方式走完整个流程（negotiate → 创建 instance → 调用被拦截的函数），不需要头显。

## 发布

改 `Cargo.toml` 的 `version` 并推到 `master`／`main`，GitHub Actions 就会跑测试、编译，创建 `v<版本>` 的 tag 和 Release，附上压缩包，更新内容是上一个 Release 以来的 commits。版本号没变，或该 tag 已存在时不会发布。

## 限制

- 只拦 `xrLocateSpace`。xrizer 0.6 只用这个；用 OpenXR 1.1 `xrLocateSpaces` 批量查询的程序不会被定住。
- 判断依据是 runtime 上报的 valid 标志。Quest 如果上报“有效但其实是猜的”姿态，这里分辨不出来。
- 手柄真的关机时，手会一直停在原地，直到 `POSE_HOLDER_MAX_HOLD_SECS` 到期（默认不会到期）。
- 设了 `POSE_HOLDER_MAX_HOLD_SECS` 时，定住时间到了 layer 会自己补发一个 profile 变更事件，让应用程序知道手柄不在了（最多慢 0.25 秒）。

## 许可

[MIT](LICENSE)。做法来自 z3y 的 [driver-pose-holder](https://github.com/z3y/driver-pose-holder)（[MIT](https://github.com/z3y/driver-pose-holder/blob/main/LICENSE.md)），代码是重新写的。最初是在 [z3y 的这条推文](https://x.com/z3y00/status/2121384801005674828)看到的。

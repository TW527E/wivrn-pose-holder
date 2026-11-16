[简体中文](README.md) · **繁體中文** · [English](README.en.md) · [日本語](README.ja.md)

# wivrn-pose-holder

[z3y/driver-pose-holder](https://github.com/z3y/driver-pose-holder) 的 Linux／WiVRn 版：控制器丟失追蹤時，把它定在最後一個有效姿勢，不讓遊戲把手收走。

原版是 SteamVR 驅動，掛在 `TrackedDevicePoseUpdated` 上。WiVRn + xrizer 的環境裡沒有 SteamVR，所以這裡做成 **OpenXR API layer**，夾在應用程式（xrizer）和 runtime（WiVRn）中間。不用改 WiVRn，也不用改 xrizer，兩邊更新後不必重新編譯。

```
VRChat → xrizer → [pose_holder] → WiVRn → Quest
```

## 做了什麼

| 攔截的函式 | 行為 |
|---|---|
| `xrLocateSpace` | 只處理控制器的 action space。位置或方向無效時，回傳最後一個有效姿勢，速度歸零。追蹤恢復後的前 5 幀仍然用舊姿勢（Quest 剛恢復時回報的姿勢還是錯的，手會瞬移一下） |
| `xrGetCurrentInteractionProfile` | 控制器休眠時 Quest 會回報「沒有 interaction profile」，xrizer 看到就把控制器標成斷線。這裡繼續回報上一個 profile，控制器保持連線 |
| `xrGetActionStatePose` | 同上，pose action 保持 active |
| `xrPollEvent` | 控制器休眠和醒來時，WiVRn 都會發出「interaction profile 變了」的事件，xrizer 收到就重建所有控制器和追蹤器（包括 Rebocap 的）。在應用程式看來 profile 沒變時，這個事件會被攔掉；真的變了（例如切到手部追蹤）才放行 |

頭顯、Rebocap 追蹤器（xdev space）和按鍵輸入都不經過這裡，原樣通過。從控制器切到手部追蹤時，profile 會變成另一個有效值，也照常通過。

## 安裝

從 Releases 下載 `wivrn-pose-holder-v*-x86_64.tar.gz`（已編譯好，不需要 Rust），解壓後執行：

```sh
./install.sh
```

或從原始碼安裝（需要 Rust）：clone 這個 repo，在資料夾裡執行同一個 `./install.sh`，腳本會自己編譯。

腳本會放兩個檔案：

```
~/.local/share/wivrn-pose-holder/libxr_pose_holder.so
~/.local/share/openxr/1/api_layers/implicit.d/XrApiLayer_pose_holder.json
```

這是 implicit layer，這個使用者開的所有 OpenXR 程式都會載入，Proton 裡的 xrizer 也是（家目錄在容器裡看得到）。裝完重開 VRChat 就生效。

移除：`./install.sh --uninstall`

## 設定

都是環境變數。要對 VRChat 設定的話，加在 Steam 啟動選項最前面。

| 變數 | 預設 | 說明 |
|---|---|---|
| `POSE_HOLDER_MIN_VALID_FRAMES` | `5` | 追蹤恢復後，要連續這麼多幀有效才採用新姿勢。恢復時手還是會跳的話調大 |
| `POSE_HOLDER_MAX_HOLD_SECS` | `0` | 最多定住幾秒，超過就放手。`0` 是一直定住（和原版一樣） |
| `POSE_HOLDER_HOLD_CONNECTION` | `1` | 設成 `0` 就只定住姿勢，不管控制器的連線狀態 |
| `DISABLE_XR_APILAYER_POSE_HOLDER` | | 設成任何值就不載入這個 layer |

## 確認有沒有載入

layer 會寫到 stderr：

```
[pose_holder] active (min_valid_frames=5, max_hold=forever, hold_connection=true)
[pose_holder] tracking lost on action 0x… path 0x…, holding last pose
[pose_holder] tracking recovered on action 0x… path 0x…
```

VRChat 的 stderr 可以用啟動選項 `PROTON_LOG=1 %command%` 存到 `~/steam-438100.log`。也可以用 `XR_LOADER_DEBUG=info` 跑任何 OpenXR 程式，看有沒有 `succeeded loading layer XR_APILAYER_TW527E_pose_holder`。

## 測試

```sh
cargo test
```

`tests/mock_runtime.rs` 用一個假的 runtime，照 OpenXR loader 的方式走完整個流程（negotiate → 建立 instance → 呼叫被攔截的函式），不需要頭顯。

## 發佈

改 `Cargo.toml` 的 `version` 並推到 `master`／`main`，GitHub Actions 就會跑測試、編譯，建立 `v<版本>` 的 tag 和 Release，附上壓縮檔，更新內容是上一個 Release 以來的 commits。版本號沒變，或該 tag 已存在時不會發佈。

## 限制

- 只攔 `xrLocateSpace`。xrizer 0.6 只用這個；用 OpenXR 1.1 `xrLocateSpaces` 批次查詢的程式不會被定住。
- 判斷依據是 runtime 回報的 valid 旗標。Quest 如果回報「有效但其實是猜的」姿勢，這裡分辨不出來。
- 控制器真的關機時，手會一直停在原地，直到 `POSE_HOLDER_MAX_HOLD_SECS` 到期（預設不會到期）。
- 設了 `POSE_HOLDER_MAX_HOLD_SECS` 時，定住時間到了 layer 會自己補發一個 profile 變更事件，讓應用程式知道控制器不在了（最多慢 0.25 秒）。

## 授權

[MIT](LICENSE)。做法來自 z3y 的 [driver-pose-holder](https://github.com/z3y/driver-pose-holder)（[MIT](https://github.com/z3y/driver-pose-holder/blob/main/LICENSE.md)），程式碼是重新寫的。最初是在 [z3y 的這則推文](https://x.com/z3y00/status/2121384801005674828)看到的。

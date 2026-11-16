[简体中文](README.md) · [繁體中文](README.zh-TW.md) · [English](README.en.md) · **日本語**

# wivrn-pose-holder

[z3y/driver-pose-holder](https://github.com/z3y/driver-pose-holder) の Linux／WiVRn 版です。コントローラーのトラッキングが切れたとき、最後の有効な姿勢で固定し、ゲーム側で手が飛ばされないようにします。

元は SteamVR ドライバーで、`TrackedDevicePoseUpdated` をフックしています。WiVRn + xrizer の環境には SteamVR がないため、こちらはアプリ（xrizer）とランタイム（WiVRn）の間に入る **OpenXR API レイヤー**として作りました。WiVRn にも xrizer にもパッチを当てないので、どちらを更新しても再ビルドは不要です。

```
VRChat → xrizer → [pose_holder] → WiVRn → Quest
```

## 動作

| フックする関数 | 動作 |
|---|---|
| `xrLocateSpace` | コントローラーの action space だけを対象にします。位置か向きが無効なとき、最後の有効な姿勢を返し、速度は 0 にします。トラッキング復帰後の最初の 5 フレームも古い姿勢のままです（Quest は復帰直後にまだ誤った姿勢を返し、手が一瞬飛ぶため） |
| `xrGetCurrentInteractionProfile` | コントローラーがスリープすると Quest は「interaction profile なし」を返し、xrizer はコントローラーを切断扱いにします。レイヤーは直前の profile を返し続け、接続状態を保ちます |
| `xrGetActionStatePose` | 同様に、pose action を active のまま保ちます |
| `xrPollEvent` | コントローラーのスリープ時と復帰時に、WiVRn は「interaction profile が変わった」イベントを送り、xrizer はそのたびにコントローラーとトラッカー（Rebocap のものを含む）をすべて作り直します。アプリから見た profile が変わっていない場合はこのイベントを捨て、本当に変わったとき（ハンドトラッキングへの切り替えなど）だけ通します |

ヘッドセット、Rebocap トラッカー（xdev space）、ボタン入力には手を加えず、そのまま通します。コントローラーからハンドトラッキングに切り替えた場合は profile が別の有効な値になるので、それもそのまま通ります。

## インストール

Releases から `wivrn-pose-holder-v*-x86_64.tar.gz` をダウンロードして展開し（ビルド済み、Rust 不要）、次を実行します。

```sh
./install.sh
```

ソースから入れる場合（Rust が必要）は、この repo を clone して同じ `./install.sh` を実行します。スクリプトが先にビルドします。

スクリプトは次の 2 つのファイルを置きます。

```
~/.local/share/wivrn-pose-holder/libxr_pose_holder.so
~/.local/share/openxr/1/api_layers/implicit.d/XrApiLayer_pose_holder.json
```

implicit レイヤーなので、このユーザーが起動する OpenXR プログラムすべてに読み込まれます。Proton 内の xrizer も同じです（ホームディレクトリはコンテナ内から見えます）。インストール後に VRChat を再起動すると有効になります。

アンインストール：`./install.sh --uninstall`

## 設定

設定はすべて環境変数です。VRChat に設定する場合は、Steam の起動オプションの先頭に書きます。

| 変数 | 既定値 | 説明 |
|---|---|---|
| `POSE_HOLDER_MIN_VALID_FRAMES` | `5` | トラッキング復帰後、この数だけ連続して有効なフレームが続いてから新しい姿勢を使います。復帰時にまだ手が飛ぶ場合は増やしてください |
| `POSE_HOLDER_MAX_HOLD_SECS` | `0` | 固定する最長時間（秒）。超えると固定をやめます。`0` は無期限（元のドライバーと同じ） |
| `POSE_HOLDER_HOLD_CONNECTION` | `1` | `0` にすると姿勢だけを固定し、コントローラーの接続状態には触れません |
| `DISABLE_XR_APILAYER_POSE_HOLDER` | | 何か値を設定すると、このレイヤーを読み込みません |

## 読み込まれているかの確認

レイヤーは stderr に出力します。

```
[pose_holder] active (min_valid_frames=5, max_hold=forever, hold_connection=true)
[pose_holder] tracking lost on action 0x… path 0x…, holding last pose
[pose_holder] tracking recovered on action 0x… path 0x…
```

VRChat の stderr は、起動オプション `PROTON_LOG=1 %command%` で `~/steam-438100.log` に保存できます。任意の OpenXR プログラムを `XR_LOADER_DEBUG=info` 付きで実行し、`succeeded loading layer XR_APILAYER_TW527E_pose_holder` が出るかを見る方法もあります。

## テスト

```sh
cargo test
```

`tests/mock_runtime.rs` は偽のランタイムを相手に、OpenXR ローダーと同じ手順（negotiate → instance 作成 → フックした関数の呼び出し）を通します。ヘッドセットは不要です。

## リリース

`Cargo.toml` の `version` を変更して `master` か `main` に push すると、GitHub Actions がテストとビルドを行い、`v<バージョン>` のタグと Release を作成してアーカイブを添付します。リリースノートは前回の Release 以降の commit 一覧です。バージョンが変わっていない場合や、タグがすでにある場合は公開しません。

## 制限

- フックするのは `xrLocateSpace` だけです。xrizer 0.6 はこれしか使いませんが、OpenXR 1.1 の一括取得 `xrLocateSpaces` を使うプログラムでは固定されません。
- 判定はランタイムが返す valid フラグに基づきます。Quest が推定にすぎない姿勢を「有効」として返した場合は見分けられません。
- コントローラーの電源を本当に切った場合、`POSE_HOLDER_MAX_HOLD_SECS` が切れるまで手はその場に残ります（既定では切れません）。
- `POSE_HOLDER_MAX_HOLD_SECS` を設定している場合、固定時間が切れるとレイヤーが自分で profile 変更イベントを送り、コントローラーがなくなったことをアプリに伝えます（最大 0.25 秒遅れ）。

## ライセンス

[MIT](LICENSE)。手法は z3y の [driver-pose-holder](https://github.com/z3y/driver-pose-holder)（[MIT](https://github.com/z3y/driver-pose-holder/blob/main/LICENSE.md)）によるもので、コードは新しく書いています。きっかけは [z3y のこのポスト](https://x.com/z3y00/status/2121384801005674828)です。

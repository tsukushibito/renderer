# M1: Device・単一キューの確認結果

確認日: 2026-11-12。対象課題: `renderer-7ro`。
設計判断は[ADR 0003](adr/0003-vulkan-device-bootstrap.md)、
現行の実行コマンドは[開発手順](development.md)を参照する。
M1全体ではなく、デバイスの能力照会からDevice・単一キューの作成と解放までの記録である。

## 実装した範囲

既存のInstance実装を`vulkan/instance.rs`へ移し、`vulkan/device.rs`に
能力照会、自動選択、必須featureの明示的な有効化、Deviceとキューの所有を追加した。
公開API、汎用Backend trait、buffer／image、submit、RenderGraph、表示は追加していない。

適合候補は専用GPU、内蔵GPU、仮想GPU、その他、CPU型の順に選び、同順位は列挙順とする。
CPU型も最後の候補として利用する。選択後のDevice作成が失敗した場合は、
選択デバイスと元の原因を報告して停止し、別デバイスへの再試行は行わない。
DeviceはInstanceを所有し、Device、debug messenger、Instanceの順に解放する。
GPUへ命令を投入していないため、解放前の`vkDeviceWaitIdle`は追加していない。

## 実行環境と選択結果

| 項目 | 今回の確認 |
|---|---|
| OS / target | Windows x64 / `x86_64-pc-windows-msvc` |
| Rust | rustc 1.99.0 (`b940084d7`) |
| ローカルSDK | 1.4.341.1。調査に`C:/VulkanSDK/1.4.341.1/Bin/vulkaninfoSDK.exe --summary`を使用 |
| Instance API / loader | 要求1.3.0 / 問い合わせ結果1.4.341 |
| 検証 | `VK_LAYER_KHRONOS_validation`（報告API 1.4.341）、`VK_EXT_debug_utils`必須 |
| 選択デバイス | NVIDIA GeForce RTX 3060、`DISCRETE_GPU`、CPU fallbackなし |
| Physical Device API | variant 0、1.4.341 |
| vendor / device ID | `0x10de` / `0x2487` |
| driver | name `NVIDIA`、info `610.88`、ID `NVIDIA_PROPRIETARY` |
| driverVersionの生値 | `2559967232`。汎用のAPIバージョンとして解釈しない |
| キュー | Graphics＋Compute対応family 0、index 0、count 1 |
| 明示的に有効化したfeature | `timelineSemaphore`、`synchronization2`、`dynamicRendering` |

## 実行した確認

| 確認 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | 成功 |
| CPU経路のworkspace / all-targets / no-default-featuresのcheckとclippy (`-D warnings`) | 成功 |
| CPU経路の通常test | 成功、テスト0件。Vulkanを初期化しない |
| Vulkan feature付きのall-targets checkとworkspace clippy (`-D warnings`) | 成功 |
| Vulkan feature付きの通常test | CPUテスト20件成功、実Vulkan試験3件はignored |
| 明示実行 `vulkan_device_smoke` | 成功。RTX 3060でInstance・Device・Queueの作成と解放を4回反復 |
| 明示実行 `vulkan_instance_smoke` | 移動後のパスで成功。既存の作成・解放とINFO受信を4回反復 |
| 明示実行 `vulkan_instance_partial_failure` | 移動後のパスで成功。既存のホスト側失敗注入と解放を4回反復 |
| 実Vulkan試験の終了後集計 | 全反復でwarning 0、error 0、callback内部失敗0。例外登録なし |
| `git diff --check`、相対文書リンク、`mod.rs`不在 | 確認済み |

明示実行コマンド:

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::device::tests::vulkan_device_ -- --ignored --nocapture
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::instance::tests::vulkan_instance_ -- --ignored --nocapture
```

新しいCPU試験では、不足条件、候補ごとの理由、デバイス種別の優先順位、CPU型の最終候補、
同順位の選択、Graphics＋Compute対応familyの選択を確認した。
本番でも使用するDevice作成設定の組み立てを通して、必須3 featureと単一キューの要求を確認した。
最小限の試験用ash関数テーブルでは、実際の作成処理へ渡すデバイス・family・キュー数、
取得するqueue index、作成失敗の原因保持、Device→Instanceの解放順序と回数を確認した。

## 未検証の範囲と次の作業

実行結果はWindowsでのローカル確認であり、Linux実行やリモートCIの結果は含めない。
Windows／Linuxの既存CI構成は維持した。
複数GPUの選択とCPU型fallbackはCPU試験で確認しており、該当する実環境での実行は未検証である。
Device作成失敗はCPUの関数テーブルで注入しており、実ドライバーのOOM再現とは扱わない。
実GPU受入試験はCPU型を選んだ場合に失敗し、ソフトウェア実装上の成功で代替しない。

この試験の成功は、GPUコマンド実行、転送、readback、GPU同期、描画の成功を意味しない。
Synchronization Validationの明示設定や性能計測も対象外である。
次は`renderer-yr6`で、VMAを使ったbufferの所有・map・解放を実装する。

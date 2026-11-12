# M1: VMA 2D imageの確認結果

- 確認日: 2026-11-12（JST）
- 対象課題: `renderer-9wa`
- 契約: [ADR 0006](adr/0006-vma-image-ownership.md)

## 確認環境

Windows x64 / MSVC、Rust 1.99.0、Cargo 1.99.0で確認した。
調査には固定版の`C:/VulkanSDK/1.4.341.1/Bin/vulkaninfoSDK.exe --summary`を使用した。

| 項目 | 実測値 |
|---|---|
| GPU | NVIDIA GeForce RTX 3060 / DISCRETE_GPU |
| vendor / device ID | `0x10de` / `0x2487` |
| driver名 / info | NVIDIA / 610.88 |
| ローダー / 物理デバイスAPI | 1.4.341 / 1.4.341（variant 0） |
| Instance / VMAへ指定したAPI | 1.3 |
| Validation LayerのAPI | 1.4.341 |
| imageのformat / tiling / usage | R8G8B8A8_UNORM / OPTIMAL / TRANSFER_SRC・TRANSFER_DST |
| mip / layer / sample数 | 各1 |
| imageのメモリ | type 1、DEVICE_LOCAL |

この組み合わせの能力照会は、maxExtent `32768 × 32768 × 1`、maxMipLevels `16`、
maxArrayLayers `2048`、sampleCounts `1 / 2 / 4 / 8`、maxResourceSize `1099511627776` bytesを返した。
これらは報告された上限であり、その大きさを実際に確保できたという結果ではない。

## 実行した確認

CPU経路の全target check、clippy（`-D warnings`）、testと、Vulkan featureの全target check、
clippy（`-D warnings`）、通常test、fmt checkが成功した。
コマンドは[開発手順](development.md)に記載している。
Vulkan featureの通常試験は**35 passed / 6 ignored**、doc-testは0件。
featureなしの試験は0件で、Vulkanの初期化を行わない。

追加したCPU試験3件は、対象外の寸法・format・usageを問い合わせ前に拒否すること、
実作成と同じ条件で能力を照会して上限超過時の作成を抑止すること、未対応と一般的な問い合わせ・
作成失敗を区別して元の`vk::Result`と画像条件を保持することを確認する。
境界には小さなクロージャを使い、VMA AllocationやVulkan全体のfakeは作っていない。

実Vulkan試験は次で明示実行した。

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan:: -- --ignored --nocapture
```

既存5件と新しいimage試験の**6件すべてが成功**した。
各試験は初期化・終了を4回反復し、全24回の終了報告で
**Validation warning 0 / error 0 / callback failure 0**だった。誤検出の例外はない。
既存のbuffer転送でも、257 bytesの全byte一致を計12回確認した。

新しいimage試験では、次の3サイズを各4回、計12画像作成・解放した。

| 要求寸法 | VMA Allocationサイズ |
|---|---|
| 1 × 1 | 512 bytes |
| 17 × 13 | 2048 bytes |
| 257 × 3 | 8704 bytes |

Allocationサイズを画素のpacked byte数と同一視していない。
初期layoutはUNDEFINEDで、imageの内容は未初期化としてログに残した。
寸法0とDevice停止後の作成を拒否し、Allocatorの操作用ハンドルを先に破棄しても
画像が依存所有者を保持すること、最後の画像の解放で所有者へのWeak参照が失効することを確認した。
Device停止はアプリケーション内部状態の切り替えであり、native DeviceLostを偽装していない。

imageとbuffer転送の試験では、Instance作成直後のValidation Layer起動INFOから、
実効SyncVal有効・無効化された検証項目なしを計8回確認した。
既存の設定確認を共通のtest専用helperへ移し、image試験単独でも同じ判定を行う。
別プロセスで`VK_KHRONOS_VALIDATION_VALIDATE_SYNC=false`を指定した負例では、
image試験がDevice作成前に期待どおり受入を拒否した。
これは検証設定の判定確認であり、不正なVulkan操作を実行した結果ではない。

実行証拠はGit管理外の`.tools/renderer-9wa-environment.log`、
`.tools/renderer-9wa-vulkan.log`、`.tools/renderer-9wa-syncval-disabled.log`へ保存した。
入力や停止状態を拒否したrenderer自身のERRORログは、Validationのwarning / errorと区別する。

## 未検証範囲と次の段階

今回のimage試験は作成・所有・解放だけを対象とし、GPUへ命令を投入していない。
imageの画素一致、buffer → image → readback、row pitch、レイアウト遷移、ImageView、
シェーダー参照、描画・Compute性能は未検証である。
作成・破棄の成功を画像転送や性能の確認結果には数えない。

実driverのOOM、DeviceLost、命名失敗や未対応Deviceは再現していない。
問い合わせ・作成失敗はCPU境界試験、作成後の後始末は具体的な所有型と独立コードレビューの確認範囲である。
CPU型fallback、複数GPU、Linuxローカル実行・リモートCIは今回未確認。

残るM1ではbuffer → image → readbackの転送・レイアウト遷移・画素照合を通し、
M1全体の完了条件を確認する。Swapchainは[実装方針](renderer-implementation-policy.md)のM3で扱う。

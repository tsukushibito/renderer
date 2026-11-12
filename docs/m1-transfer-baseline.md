# M1: buffer転送・完了待ちの確認結果

- 確認日: 2026-11-12（JST）
- 対象課題: `renderer-dj3`
- 契約: [ADR 0005](adr/0005-single-queue-buffer-transfer.md)

## 確認環境

Windows x64 / MSVC、Rust 1.99.0、Cargo 1.99.0で確認した。
調査には固定版の`C:/VulkanSDK/1.4.341.1/Bin/vulkaninfoSDK.exe --summary`を使用した。

| 項目 | 実測値 |
|---|---|
| GPU | NVIDIA GeForce RTX 3060 / DISCRETE_GPU |
| vendor / device ID | `0x10de` / `0x2487` |
| driver名 / info | NVIDIA / 610.88 |
| driverVersion（生値） | `2559967232` |
| ローダー / 物理デバイスAPI | 1.4.341 / 1.4.341（variant 0） |
| Instance / VMAへ指定したAPI | 1.3 |
| Validation LayerのAPI | 1.4.341 |
| queue family / index | 0 / 0（Graphics＋Compute、1本） |
| bufferの要求 / Allocationサイズ | 257 / 272 bytes |
| Uploadのメモリ | type 3、HOST_VISIBLE / HOST_COHERENT |
| DeviceTransferのメモリ | type 1、DEVICE_LOCAL |
| Readbackのメモリ | type 4、HOST_VISIBLE / HOST_COHERENT / HOST_CACHED |

`driverVersion`はVulkan API形式として汎用デコードせず、生値とdriver自身の情報を区別した。

## 実行した確認

CPU経路のcheck、clippy、test、Vulkan featureの全target check、clippy、通常test、
`cargo fmt --all -- --check`が成功した。コマンドは[開発手順](development.md)に記載している。
Vulkan featureの通常試験は**32 passed / 5 ignored**、doc-testは0件。
featureなしの試験は0件で、Vulkanの初期化を行わない。

追加したCPU試験は、同一所有者・用途・サイズ・Upload初期化の入力契約、
未投入とsubmit OOMの解放、wait timeout／一般エラーでの所有継続、
成功完了時の所有返却、Device loss時の成功とは区別した解放、
不確定な投入とunwindでの保持、Device停止後の通常利用拒否を対象とする。
所有状態の試験は実装で使用する保持処理とCPU上の破棄観測を使い、VMA全体を模倣しない。

実Vulkan試験は次で明示実行した。

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan:: -- --ignored --nocapture
```

既存のInstance正常・部分初期化失敗、Device、buffer試験と、新しい転送試験の
**5件すべてが成功**した。各試験は初期化・終了を4回反復し、終了後の診断もすべて
**warning 0 / error 0 / callback failure 0**だった。誤検出の例外はない。

転送試験では、257 bytesのUpload → DeviceTransfer → Readbackを実GPUで実行し、
Fenceの完了確認後に全byteを照合した。4回の初期化それぞれでパターンを変えて3回投入し、
計12回すべて一致した。同じbufferを完了後に再利用し、CommandPool・CommandBuffer・Fenceは
投入ごとに作成した。未投入の記録のキャンセルと、明示waitを省いた所有者のDropも確認した。
Allocatorハンドルを先に破棄しても投入資源が依存所有者を保持し、最後の解放で終了した。

Instance作成直後のレイヤー起動INFO（IDは`WARNING-CreateInstance-status-message`）で、
次の実効設定を4回とも確認した。IDの文字列にかかわらず重要度はINFOである。

```text
Current Enables: VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT.
Current Disables: None.
```

この確認はSDK 1.4.341系の起動報告形式を前提とする。報告がない・形式が未知・同期検証が無効・
検証項目が無効化されている場合は、設定とレイヤー版の確認を求めて転送試験を失敗させる。
別プロセスで`VK_KHRONOS_VALIDATION_VALIDATE_SYNC=false`を指定した確認では、
`Current Enables: None.`を検出してDevice作成前に期待どおり試験が失敗した。
これは受入判定の負例確認であり、通常試験の失敗や意図的な不正Vulkan操作ではない。

実行証拠はGit管理外の`.tools/renderer-dj3-environment.log`、
`.tools/renderer-dj3-vulkan.log`、`.tools/renderer-dj3-syncval-disabled.log`へ保存した。
未初期化read等を拒否する試験で出るrenderer自身のERRORログは、Validationのwarning/errorとは区別する。

## 未検証範囲と次の段階

今回のメモリはcoherentだった。non-coherentのflush/invalidate手順は既存CPU試験の範囲であり、
実機で確認したとは扱わない。実driverのOOM、Device loss、長時間停止、Drop待機予算超過と
その後の保持は再現していない。timeoutや失敗時の所有規則はCPU試験で確認した。
ゼロ時間のpollやDrop時にGPUがすでに完了している場合もあり、実GPUの未完了状態を常に強制したものではない。

Synchronization Validationはhost memory accessを追跡しないため、CPUとの寿命契約には
移動所有とCPU試験を併用する。回収不能になった資源の再回収やDevice自動復旧は実装していない。

Linuxは既存CIのビルド対象で、今回ローカル実行していない。リモートCIも未実施で、
Windows上の成功をLinuxの確認済み結果には数えない。
CPU型fallback、複数GPU、複数キュー、描画、性能測定も今回の実機確認には含めない。

dj3はbuffer転送の完了であり、M1全体の完了ではない。
残るM1ではVMA imageの作成・解放と、buffer → image → readbackの転送・レイアウト遷移を扱う。
Swapchainは[実装方針](renderer-implementation-policy.md)のM3で扱う。

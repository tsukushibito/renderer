# M1: 画像転送と基盤全体の確認結果

- 確認日: 2026-11-12（JST）
- 対象課題: `renderer-jya`
- 契約: [ADR 0007](adr/0007-image-transfer.md)

## 確認環境

Windows x64 / MSVC、Rust 1.99.0、Cargo 1.99.0で確認した。
固定版の`C:/VulkanSDK/1.4.341.1/Bin/vulkaninfoSDK.exe --summary`を使用した。

| 項目 | 実測値 |
|---|---|
| GPU | NVIDIA GeForce RTX 3060 / DISCRETE_GPU |
| vendor / device ID | `0x10de` / `0x2487` |
| driver名 / info | NVIDIA / 610.88 |
| ローダー / 物理デバイスAPI | 1.4.341 / 1.4.341（variant 0） |
| Instance / VMAへ指定したAPI | 1.3 |
| Validation LayerのAPI | 1.4.341 |
| queue family / index | 0 / 0（Graphics＋Compute、1本） |
| image | R8G8B8A8_UNORM、OPTIMAL、TRANSFER_SRC・TRANSFER_DST、mip・layer・sample各1 |
| Uploadのメモリ | type 3、HOST_VISIBLE / HOST_COHERENT |
| imageのメモリ | type 1、DEVICE_LOCAL |
| Readbackのメモリ | type 4、HOST_VISIBLE / HOST_COHERENT / HOST_CACHED |

既知のValidation設定上書き環境変数は未設定だった。
レイヤー起動INFOから実効SyncVal有効・無効化された検証項目なしを確認した。
APIによる設定要求だけを有効状態の証拠にはしていない。

## 実行した確認

CPU経路とVulkan featureの全target check、clippy（`-D warnings`）、test、fmt checkが成功した。
Vulkan featureの通常試験は**37 passed / 7 ignored**、featureなしは0件、doc-testは0件。
通常試験ではVulkanを初期化しない。再現コマンドは[開発手順](development.md)に記載した。

追加したCPU試験2件は、画像・bufferの所有者、用途、サイズ、Upload全域初期化、画像条件・layout、
packed byte数のoverflow拒否と、実際の記録に使うcopy範囲・初回／再利用barrierを確認する。
submit・wait失敗とtimeout時の保持、成功返却、Device loss、unwindの所有状態は、
共用した既存の`Retained`試験を引き継ぐ。VMAやVulkan全体のfakeは追加していない。

実Vulkan試験は次で明示実行した。

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan:: -- --ignored --nocapture
```

既存6件と新しい画像転送試験の**7件すべてが成功**した。
各試験の4回反復、計28回の終了報告で
**Validation warning 0 / error 0 / callback failure 0**だった。誤検出の例外はない。
画像作成、buffer転送、画像転送の計12回で、起動時の実効検証設定も確認した。

画像転送は次の3寸法を使い、4回の初期化それぞれで同じ資源へ3パターンを投入した。
座標、RGBAのchannel、反復ごとに変わるbyte列を使い、計36回すべてで全byteが一致した。

| 寸法 | 転送するpacked byte数 |
|---|---|
| 1 × 1 | 4 |
| 17 × 13 | 884 |
| 257 × 3 | 3084 |

初回のUNDEFINEDと、成功完了後のTRANSFER_SRC_OPTIMALを確認し、
再投入時は前回の完了済みlayoutから遷移した。
画像のnative配置をCPUから直接読まず、行長指定0のcopyでReadbackへ戻して照合した。
このpacked byte数をimageのAllocationサイズやnative row pitchと同一視していない。
既存buffer転送の257 bytes、計12回の照合も成功した。

未投入の記録のキャンセル、ゼロ時間pollと未完了の場合の再待機、完了後の再利用を確認した。
Allocatorハンドルを先に破棄しても未完了所有者が依存関係を保持し、
明示waitを省いたDropがFence結果を取得して解放した後、所有者のWeak参照が失効した。
pollやDropの時点ですでにGPUが完了している場合もあり、未完了状態を強制した試験ではない。
timeoutや完了不明時の保持はCPU状態試験の範囲である。

image・command・寸法・byte数・開始／終了layoutを記録ログへ、command・Fenceを投入ログへ残し、
native資源と転送条件の対応を追えるようにした。
実行証拠はGit管理外の`.tools/renderer-jya-environment.log`と`.tools/renderer-jya-vulkan.log`へ保存した。
未初期化readを拒否するrenderer自身のERRORログは、Validationのwarning / errorとは区別する。

## M1の完了範囲と限界

[実装方針のM1](renderer-implementation-policy.md)が要求するprivate backendの縦切りを、
今回確認した対応GPU上で満たした。

| M1の項目 | 確認する実装・結果 |
|---|---|
| Instance・Device・単一キュー | [Instance](m1-instance-baseline.md)・[Device](m1-device-baseline.md)の反復初期化・終了 |
| 検証・ラベル・診断 | 独立した診断集計、object名・command label、終了後までのwarning / error 0 |
| VMA buffer / image | [buffer](m1-buffer-baseline.md)・[image](m1-image-baseline.md)の所有・作成・解放 |
| 転送・実行・完了 | [buffer転送](m1-transfer-baseline.md)と今回の画像転送、実Fence待機、完了後の再利用 |
| 既知内容のreadback照合 | bufferの12回と画像の36回の全byte一致 |
| 失敗時の所有・後始末 | CPU境界・所有状態試験、部分初期化失敗試験、具体的所有コードの独立レビュー |

今回の実機メモリはcoherentだった。non-coherentのflush / invalidateは既存CPU試験の確認範囲で、
実機で確認したとは扱わない。実driverのOOM、DeviceLost、長時間停止、Drop待機予算超過は未再現。
失敗経路をすべて実GPU上で発生させたという意味ではない。
Synchronization Validationはhost memory accessを追跡しないため、CPUアクセスの安全性には移動所有と
CPU試験・コードレビューを併用する。回収不能資源の再回収やDevice自動復旧は実装していない。

Linuxローカル実行、リモートCI、CPU型fallback、複数GPUは今回未確認である。
部分領域、行長指定、ImageView、描画・Compute、RenderGraph、画像ファイル出力、性能測定は対象外。
M2は最小RenderGraphへ進み、Swapchain、フレームをまたぐ寿命管理はM3で扱う。
M1の成功を全GPU・全OSや将来の描画性能の保証にはしない。

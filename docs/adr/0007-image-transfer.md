# ADR 0007: 画像全体の転送と完了後の再利用

- 日付: 2026-11-12
- 状態: 採用
- 関連課題: `renderer-jya`

## 背景

[VMA imageの所有](0006-vma-image-ownership.md)と
[単一キューの転送・完了管理](0005-single-queue-buffer-transfer.md)を接続し、
Upload buffer → image → Readback bufferを通して既知の画素を照合する。
これをM1の画像転送と正常解放の確認経路とする。

## 採用案

- ユーザーの選択により、対象はRGBA8_UNORM画像全体の転送に限定する。
  optimal tiling、mip・layer・sample各1という既存の作成条件を継承する。
  buffer内の画素はR・G・B・Aの順で各1 byte、左から右、行の順に隙間なく並べる。
  色変換や画像ファイル形式への変換は行わない。
- 転送サイズは`幅 × 高さ × 4`をchecked計算する。
  UploadとReadbackはその要求サイズに一致し、対応する用途を持ち、画像も含めて同じ
  Allocator所有者に属すること、Upload全域が初期化済みであることを記録前に確認する。
  このpacked byte数はoptimal imageのAllocationサイズやnative row pitchを意味しない。
- `VkBufferImageCopy`はbuffer offset 0、bufferRowLength / bufferImageHeight 0、
  COLOR aspect、mip 0、base layer 0、layer count 1、image offset 0、depth 1とする。
  行長指定0により、bufferはimageExtentに従うtight packingとなる。
- 初回はUNDEFINEDからTRANSFER_DST_OPTIMALへ遷移し、Uploadから画像全体へcopyする。
  COPY / TRANSFER_WRITEからCOPY / TRANSFER_READへの依存とともに
  TRANSFER_SRC_OPTIMALへ遷移し、画像からReadbackへcopyする。
  最後にTRANSFER_WRITE → HOST_READの依存を記録する。
  CPU writeのflushはsubmit前、必要なinvalidateは成功待機後のbuffer `read`が担う。
- 完了した同じimageとbufferを返し、パターンを変えた再投入を可能にする。
  再利用時はTRANSFER_SRC_OPTIMALからTRANSFER_DST_OPTIMALへ遷移する。
  完了済みlayoutを記録時に先取りせず、対象Fenceの成功待機とDevice active確認後に確定する。
  未投入の記録のキャンセルはGPU上のlayoutを変えない。
- 記録済み・未完了の所有者がimage、buffer、command資源と依存所有者を保持する。
  生ハンドルの使用はこのprivate所有経路に限定し、CPUアクセスや通常の解放と競合させない。
  native command資源を参照先のimage・bufferより先に破棄する。
- buffer転送と画像転送で投入・Fence待機・Drop・失敗時の保持処理を共用する。
  `submit`と`wait`の分離、期限切れ時の再試行、元のVulkanエラーの保持、DeviceLostの扱い、
  10秒のAPI待機予算を使うDropはADR 0005の契約を継承する。
  完了不明時には資源と依存所有者を保持し、Deviceの通常利用を止める。
  Device lossや一般的な待機エラーを画素の成功として返さない。
- 寸法、転送byte数、開始・終了layoutとFenceを診断へ対応づける。
  既存のobject名・command labelを使い、通常の処理でもトラブルを調査できるログを残す。

## 後続拡張との関係

部分領域、buffer offset、行間の余白指定は後でcopy条件として追加する。
その追加だけを理由に既存の転送用画像を作り直す必要はない。
formatやusage、mipなどの作成条件の変更はADR 0006に従う。
複数mip・layerではlayout・初期化の追跡もsubresource単位へ拡張する。

この経路はprivateなM1の縦切りであり、汎用encoderやRHIの公開契約にはしない。
RenderGraphでの状態計画、ImageView、描画・Compute、フレーム並列化、Swapchainは含めない。
今回の試験で投入後すぐに待つことを、通常フレームの全面待機の規約にはしない。

## 検証

CPU試験では入力契約、サイズ計算と実際に使うcopy・layout条件を確認する。
投入・待機・Dropの所有状態は共通処理の既存試験を継承し、VulkanやVMA全体のfakeは作らない。
実Vulkan試験を明示実行し、複数寸法と座標・channel・反復ごとに変わるパターンの全byte一致、
完了後の同一資源の再利用、未投入キャンセル、ゼロ時間poll、Drop待機、Allocator先行破棄を確認する。
pollで未完了なら再待機する。GPUがすでに完了している場合もあり、timeout状態の強制はCPU試験が担う。

実効Synchronization Validationとcore validationの有効状態を起動INFOで確認し、
終了後までwarning / error / callback内部失敗の各0を要求する。誤検出の例外はない。
実driverの失敗、non-coherentメモリ、未完了状態の強制などは実施結果と区別し、
[画像転送・M1の確認結果](../m1-image-transfer-baseline.md)へ記録する。

## 一次資料

- [VkBufferImageCopyのtight packingと範囲](https://docs.vulkan.org/refpages/latest/refpages/source/VkBufferImageCopy.html)
- [vkCmdCopyBufferToImage](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdCopyBufferToImage.html)
- [vkCmdCopyImageToBuffer](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdCopyImageToBuffer.html)
- [VkImageMemoryBarrier2のlayout遷移と同期範囲](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageMemoryBarrier2.html)
- [Khronosの同期例](https://docs.vulkan.org/guide/latest/synchronization_examples.html)

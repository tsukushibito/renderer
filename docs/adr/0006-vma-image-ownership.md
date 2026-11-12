# ADR 0006: VMA 2D imageの作成と所有権

- 日付: 2026-11-12
- 状態: 採用
- 関連課題: `renderer-9wa`

## 背景

M1の[VMA buffer](0004-vma-buffer-ownership.md)と[転送基盤](0005-single-queue-buffer-transfer.md)に
続き、imageの作成・所有・解放を実装する。命令記録やレイアウト遷移を追加する前に、
作成条件の検証、Allocationと依存所有者の寿命、失敗時の後始末を確認する。

## 採用案

- ユーザーの選択により、今回はサイズ可変の転送用RGBA8_UNORMに限定する。
  2D、optimal tiling、ミップ1段、配列1層、サンプル数1、transfer source / destination、
  EXCLUSIVE共有、初期layout UNDEFINEDとする。作成時に内容の初期化を保証しない。
- `vulkan` feature配下のprivate実装とする。画像の寸法・format・usageを記述として保持し、
  現在受理する条件の検証を、画像とメモリの所有・解放から分ける。
  対応formatや用途を増やすために、公開APIや汎用RHIを先に固定しない。
- 寸法0や今回の対象外条件をnative作成前に拒否する。
  `vkGetPhysicalDeviceImageFormatProperties2`へ実作成と同じformat・type・tiling・usage・flagsを渡し、
  成功した出力のmaxExtent・maxMipLevels・maxArrayLayers・sampleCountsを検証する。
  未対応とその他の問い合わせ失敗を区別し、失敗時の出力を能力情報として使わない。
- optimal imageの実メモリサイズを画素のpacked byte数から推定しない。
  `maxResourceSize`は作成前に正確に比較できないため、native作成時のOOMも通常のエラーとして扱う。
- VMAにはAutoPreferDeviceを指定する。独自pool、強制dedicated allocation、CPU mappingは導入しない。
  作成成功直後にimageとAllocationの組を単独所有型へ収め、後続の命名・情報照会・ログ出力で
  失敗やunwindが起きても解放する。Cloneや生ハンドルを渡すAPIは提供しない。
- imageはbufferと同じ`Arc<MemoryOwner>`を保持し、Allocatorの操作用ハンドルが先に破棄されても
  VMA・Device・Instanceを維持する。最後のimageの破棄はimageとAllocationを解放してから
  所有者への参照を解放する。VMAをDeviceより先に破棄する既存のfield順序を維持する。
  `mem::forget(image)`でも参照が残り、未解放Allocationの下でVMAだけを破棄しない。
- Device停止・lost後の作成を拒否し、nativeのDeviceLostを既存の共有状態へ反映する。
  エラーには操作・画像条件と元の`vk::Result`を保持する。
  診断にはdebug object名、寸法・format・usage、対応上限と実Allocation・メモリ情報を残す。

## 後続拡張との関係

対応範囲の追加は、拡張した作成APIで必要な条件の画像を新しく作ることを意味する。
既に作成したVkImageの用途、ミップ数、配列層数、サンプル数を後から変更するものではない。
ImageViewは元の画像の作成条件に従い、画像より先に破棄される所有関係を別途実装する。
ミップ・配列対応時には、初期化と同期の状態追跡もsubresource単位へ拡張する。

今回の制限により将来の描画に追加copyや全面待機を要求する契約は設けない。
描画用・storage用などの画像は、それぞれ必要な用途で直接作成する。
性能の実測は該当する描画・Compute経路を実装した段階で行う。

imageとAllocationの一対一所有は今回のprivate実装に限定する。
[実装方針9.1](../renderer-implementation-policy.md)に従い、将来のaliasingを妨げる
公開・恒久契約にはしない。共有メモリ導入時にはAllocationの所有と画像の破棄を分ける。

## 検証と範囲

CPU試験は入力契約、返却能力の上限、未対応時の作成抑止、問い合わせ・作成失敗の原因保持を対象とする。
置き換えは必要な境界に限定し、VMAやVulkan全体のfakeを作らない。
明示実行の実Vulkan試験では複数サイズの作成・解放を反復し、Allocatorハンドル先行破棄と
最後のimageによる依存所有者の解放を確認する。
終了後までValidation warning / error / callback内部失敗の各0を必須とし、CPU型は実GPU受入に数えない。

ImageView、画像へのCPUアクセス、GPU転送、レイアウト遷移、画素照合、描画、RenderGraphは今回の対象外。
GPUへ使用を広げる段階で、記録から完了までの保持と同期・初期化状態を追加する。
作成できたことをGPU転送・画素readbackの成功と扱わない。
実行結果と未検証範囲は[imageの確認結果](../m1-image-baseline.md)に記録する。

## 一次資料

- [VkImageCreateInfo](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageCreateInfo.html)
- [vkGetPhysicalDeviceImageFormatProperties2](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPhysicalDeviceImageFormatProperties2.html)
- [VkImageFormatPropertiesとmaxResourceSize](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageFormatProperties.html)
- [ImageViewが継承する作成条件](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageViewCreateInfo.html)
- [vk-mem 0.5.0の作成・解放API](https://docs.rs/vk-mem/0.5.0/vk_mem/trait.Alloc.html)

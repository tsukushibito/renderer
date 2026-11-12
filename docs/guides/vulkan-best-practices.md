# Vulkan実装・レビューのベストプラクティス

調査日: **2026-11-12**。対象は、このRendererで使うVulkan 1.3のPC向け基盤と、
RenderGraph・描画・Compute・将来のPresentationへ進む際の実装判断である。
Khronosの仕様・Guide・Samples、Vulkan Validation Layers、VMA、GPUベンダーの一次資料を参照する。
全拡張を列挙する文書ではなく、正しさ・寿命・性能に影響する契約を分野別に整理する。

現在の実装契約は[Vulkanバックエンドの設計](../design/vulkan.md)、
全体方針は[実装方針](../design/implementation-policy.md)、
実行方法は[開発手順](development.md)と[ツールガイド](tooling.md)、
実測結果は[現在の検証状況](../verification/current.md)を正とする。
本書の調査は、新しいGPU機能の実装・実機検証を意味しない。

Vulkan関連の実装・レビューでは、本書の該当内容に従う。
適用区分、一般推奨から異なる選択をする際の記録、機材依存の最適化の採用条件は
[AGENTS.mdの実装規約](../../AGENTS.md#vulkan-implementation-and-review)を参照する。

分野別の参照先:

- [能力の照会と初期化](#capabilities)
- [メモリ・資源の作成と所有](#memory)
- [同期・キュー・GPU完了](#synchronization)
- [Shader・SPIR-V・CPU/GPU ABI](#shader)
- [Descriptor・pipeline・キャッシュ](#bindings)
- [Compute・subgroup・shaderの信頼境界](#compute)
- [描画・画像・Attachment（M2で導入）](#raster)
- [WSI・Swapchain（M3で導入）](#wsi)
- [検証・診断・性能測定](#measurement)
- [最適化の判断と後続機能](#optimization)
- [本Rendererへの適用とレビュー基準](#application)
- [資料の選び方と更新](#sources)

## 読み方と根拠の優先順位

| 区分 | 意味 | 採用方法 |
|---|---|---|
| **仕様要件** | VulkanのValid Usage、同期・寿命・featureなどの契約 | 使用するAPI・拡張の条件を満たす。性能測定を理由に破らない |
| **VMAの契約** | 利用版allocatorの同期・所有・API条件 | Vulkanの要件とともに必ず満たす |
| **一般推奨** | 公式Guide・Samples等の設計・調査上の助言 | 原則採用する。異なる選択は理由・代案・リスク・根拠を記録する |
| **条件付き最適化** | 特定GPU・ドライバー・負荷での改善候補 | 対象環境で正しさとCPU/GPU時間・メモリ・latencyを測る |
| **本Rendererの方針** | 単一キュー、VMA、上限付き再利用、厳格な検証など | 現行方針を守る。変更は理由を現在の設計とBeadsへ明記する |

APIの可否は仕様・reference pageで確認し、Guide/Samplesは使い方の説明として読む。
ベンダー推奨の対象世代・記事更新日を保持し、他社GPUへの普遍的な保証へ読み替えない。
`latest`のリンクは将来更新される。実装時には使用中のSDK・ash・vk-memと、
記載機能の提供core version／extension／featureを照合する。
現行のSDKやRust依存の更新は本書によって自動的に承認・要求されない。
[Khronos Registryの仕様公開方針](https://registry.khronos.org/vulkan/)、
[Vulkanのversionと移植](https://docs.vulkan.org/guide/latest/versions.html)。

<a id="capabilities"></a>

## 能力の照会と初期化

**仕様要件:** ローダー／InstanceのAPI versionとPhysicalDeviceのAPI versionを区別する。
ヘッダーに型やenumが存在することから実行可能とは判断せず、各objectに対応するversionの範囲で使う。
`driverVersion`の符号化もVulkan API versionと同じと仮定しない。
[VkPhysicalDeviceProperties](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceProperties.html)。

**仕様要件:** 拡張名の列挙、拡張の有効化、feature bitの対応、featureの有効化、
今回の処理での採用を別々に管理する。core昇格後は該当core経路を使えるが、
任意featureが無条件に有効になるわけではない。
照会結果をそのまま全有効化せず、使用する機能だけをDevice作成へ渡すのを本Rendererの基準とする。
[featureの照会・有効化](https://docs.vulkan.org/guide/latest/enabling_features.html)、
[拡張とcore昇格](https://docs.vulkan.org/guide/latest/enabling_extensions.html)。

**仕様要件:** `pNext`の構造・寿命・`sType`を正しく保ち、同じ機能の個別構造体と
`VkPhysicalDeviceVulkan12Features`／`Vulkan13Features`等の集約構造体を重複してつながない。
`VkPhysicalDeviceFeatures2`をDevice作成のchainに渡す場合、`pEnabledFeatures`はNULLにする。
[VkDeviceCreateInfo](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceCreateInfo.html)。

**一般推奨:** 次の情報をまとめて収集し、対応不足と照会失敗を区別する。
format対応は単独のformat bitだけでなく、実際のtype・tiling・usage・flags・sample数・extentの
組み合わせに対して確認する。GPU種別による選択順位は製品方針であり、
最大性能を保証するVulkan規則ではない。
[能力照会の分類](https://docs.vulkan.org/guide/latest/querying_extensions_features.html)、
[画像作成条件の照会](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPhysicalDeviceImageFormatProperties2.html)。

| 確認領域 | 実装前に必要な情報 |
|---|---|
| キュー | Graphics/Compute/Transfer、queue数、将来のsurfaceごとのpresent対応、timestamp対応 |
| リソース | usage・format・tilingの組、サイズ・mip・layer・sample数、メモリ要件 |
| バインディング | descriptorの種別別・stage別上限、push constant範囲、buffer offset整列 |
| Compute | workgroup各軸・総invocation・shared memory・subgroup能力 |
| ホストアクセス | memory type/heap、coherency、cacheability、nonCoherentAtomSize |

上限は都合のよい定数で固定しない。必須条件の不足は診断し、対応外か明示的な別経路へ進む。
[VkPhysicalDeviceLimits](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceLimits.html)。

**仕様要件:** `VkResult`の正のstatusをすべて「処理完了」と扱わない。
`NOT_READY`、`TIMEOUT`、列挙の`INCOMPLETE`、WSIの`SUBOPTIMAL_KHR`、
pipeline作成の`PIPELINE_COMPILE_REQUIRED`等は、各APIの契約に従って分岐する。
元の結果・操作・resource／pass／Deviceを保ったエラーを返す。
[VkResult](https://docs.vulkan.org/refpages/latest/refpages/source/VkResult.html)。

**将来の移植:** `VK_KHR_portability_enumeration`とInstanceの列挙flagが必要な実装、
`VK_KHR_portability_subset`を報告したDeviceではその拡張の有効化が必要な条件を確認する。
subsetの追加制約を照会し、MoltenVK経由の成功をネイティブMetalバックエンドの検証と数えない。
Profilesは要求能力をまとめる補助手段であり、このリポジトリへ導入済みとは扱わない。
[portability enumeration](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_portability_enumeration.html)、
[portability subset](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_portability_subset.html)、
[Vulkan Profiles](https://docs.vulkan.org/guide/latest/vulkan_profiles.html)。

<a id="memory"></a>

## メモリ・資源の作成と所有

この節の調査日は2026-11-12。現行`vk-mem 0.5.0`の同梱headerはVMA 3.3.0であり、
公開サイトの最新版にあるAPIを、そのままRust bindingで利用可能とみなさない。
ローカルheaderの先頭と、[VMA 3.3.0の固定ソース](https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator/v3.3.0/include/vk_mem_alloc.h)を照合した。

### メモリの分類を要求と選好に分ける

**仕様要件:** heapは容量を持つ物理メモリ領域、memory typeはheapへの参照とアクセス特性である。
複数typeが同じheapを共有し得るため、typeごとの容量を足し合わせてVRAM容量としない。
[VkPhysicalDeviceMemoryProperties](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceMemoryProperties.html)

| 特性 | 意味と判断 |
|---|---|
| `DEVICE_LOCAL` | deviceから効率よくアクセスできる。専用VRAMであることやCPUからmap不能であることは意味しない |
| `HOST_VISIBLE` | CPUからmap可能。GPUとの同時アクセスが安全になる意味はない |
| `HOST_COHERENT` | host cacheのflush/invalidateを明示する必要がない。完了待ちや競合排除は必要 |
| `HOST_CACHED` | host read向けcache特性。coherentとは独立に確認する |
| `LAZILY_ALLOCATED` | device専用でbackingが遅延確保され得る。一般的な永続テクスチャ向けの指定ではない |

特性の正確な定義は[VkMemoryPropertyFlagBits](https://docs.vulkan.org/refpages/latest/refpages/source/VkMemoryPropertyFlagBits.html)を基準とする。

**一般推奨:** VMAの`AUTO`／`AUTO_PREFER_DEVICE`／`AUTO_PREFER_HOST`を出発点とし、
必要なhostアクセスには`HOST_ACCESS_SEQUENTIAL_WRITE`または`HOST_ACCESS_RANDOM`を指定する。
`PREFER_*`は要求ではない。実際に得たtypeの特性を確認する。
[VMA: Choosing memory type](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/choosing_memory_type.html)

**条件付き最適化:** 専用GPUの大量・反復アクセスではdevice-local資源へstaging copyする経路が有力で、
UMAではhost-visibleかつdevice-localなtypeにより余分なcopyを避けられる場合がある。
GPU種別名だけで経路を固定せず、実際のtype、転送量、更新頻度と計測で選ぶ。
[Khronos: Memory Allocation](https://docs.vulkan.org/guide/latest/memory_allocation.html)

ReBAR等でCPUから見えるVRAMも、CPU書き込みの帯域やcache特性が無制限になるわけではない。
write-combined領域には逐次書き込みを使い、readやread-modify-writeを避ける。
直接更新とstaging copyの比較にはCPU時間だけでなくGPU accessと転送競合も含める。
[VMA: Recommended usage patterns](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/usage_patterns.html)

### 要件照会、suballocation、専用Allocation

**仕様要件:** 資源ごとに返る`size`、`alignment`、`memoryTypeBits`を満たしてbindする。
画像の確保量を`width × height × bytes_per_pixel`から決定しない。
packed byte数はupload/readbackの論理サイズであり、optimal tilingや内部metadataを含む確保量とは異なる。
[VkMemoryRequirements](https://docs.vulkan.org/refpages/latest/refpages/source/VkMemoryRequirements.html)

**仕様要件:** Vulkan 1.3のbuffer作成前には、`VkPhysicalDeviceMaintenance4Properties::maxBufferSize`
を照会し、要求sizeが非ゼロかつ上限以下であることを確認する。
このsizeのValid Usageは`maintenance4` featureを有効にした場合だけの条件ではない。
CPUで表現できるサイズ、staging容量、allocation成功の可否とは別の上限であり、
VMAに渡す前に通常の入力エラーとして拒否する。
[VkBufferCreateInfo: size-06409](https://docs.vulkan.org/refpages/latest/refpages/source/VkBufferCreateInfo.html)、
[VkPhysicalDeviceMaintenance4Properties](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceMaintenance4Properties.html)。

format、image type、tiling、usage、flagsの実際の組み合わせを照会し、対応extent・mip・layer・sample数を確認する。
個別のformat featureがあるだけで、任意の組み合わせの作成成功を保証しない。
[vkGetPhysicalDeviceImageFormatProperties2](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPhysicalDeviceImageFormatProperties2.html)

**一般推奨:** 通常はVMAが用意するtype別の既定poolとsuballocationを使う。
custom poolは固定容量、寿命や配置の分離などの具体的な要件に対して導入する。
poolの事前確保・細分化により、余剰確保や利用できない空き領域を増やす場合もある。
[VMA: Custom memory pools](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/custom_memory_pools.html)

**仕様要件:** `requiresDedicatedAllocation`は必須条件、`prefersDedicatedAllocation`は実装の選好である。
大きなrender target等をdedicatedにする判断と、全資源で必須とする判断を分ける。
まずVMAの要件照会・判断を使い、独自の固定サイズ閾値は測定理由なしに追加しない。
[VkMemoryDedicatedRequirements](https://docs.vulkan.org/refpages/latest/refpages/source/VkMemoryDedicatedRequirements.html)

**仕様要件:** alignmentには異なる目的がある。bind offset、buffer/imageの隣接配置、
descriptorのdynamic offset、CPU/GPU構造体ABI、copy制約、cache atomを一つの定数で代用しない。
`maxMemoryAllocationCount`も資源数そのものの上限とは異なる。
[VkPhysicalDeviceLimits](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceLimits.html)

### map、cache制御、CPU/GPU間の競合

**一般推奨:** 頻繁にCPU更新するupload領域はpersistent mapを候補にする。
map中のままGPUから利用でき、map/unmapだけではcacheはflush/invalidateされない。
`MAPPED`だけではhost-visibleなtypeを保証しない。VMAが管理するallocationはVMA経由でmapし、
共有`VkDeviceMemory`への二重native mapを避ける。
[VMA: Memory mapping](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/memory_mapping.html)

現行同梱VMAはmapping hysteresisを既定で有効にし、短期間のmap/unmapに対してnative mappingを保持し得る。
scopedなVMA map呼び出し数を、そのままdriverのmap呼び出し数や性能損失へ換算しない。
[VMA 3.3.0固定ソース](https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator/v3.3.0/include/vk_mem_alloc.h)

**仕様要件:** 非coherentなCPU書き込みは、hostの書き込みを完了させてからflushする。
unmapはflushを代替しない。flushはcache availabilityの操作であり、GPU使用中の領域へCPUが書く権利を与えない。
特にnon-temporal storeを使う独自高速化は、host memory modelの順序も保証する。
[vkFlushMappedMemoryRanges](https://docs.vulkan.org/refpages/latest/refpages/source/vkFlushMappedMemoryRanges.html)

**仕様要件:** 非coherentなreadbackは、GPU writeからhost readへの適切な依存、実完了を示すhost wait、
invalidate、CPU readの順で扱う。invalidate自体はdevice operationを待たない。
未flushのCPU dirty dataを含む範囲へのinvalidateは内容を未定義にし得る。
[vkInvalidateMappedMemoryRanges](https://docs.vulkan.org/refpages/latest/refpages/source/vkInvalidateMappedMemoryRanges.html)

native flush/invalidateのoffsetとsizeは、`nonCoherentAtomSize`とmapped範囲の条件を満たす。
末尾の例外や`VK_WHOLE_SIZE`の意味を含め、単純な丸めでmapping外を指定しない。
VMAのallocation相対APIへ渡すoffsetと、native `VkDeviceMemory`先頭相対offsetを混同しない。
[VkMappedMemoryRange](https://docs.vulkan.org/refpages/latest/refpages/source/VkMappedMemoryRange.html)

**本Renderer:** 一つの共有buffer内に複数leaseを置く場合、VMAがallocation同士を整列していても、
内部lease同士は自動的にcache atomから分離されない。配置offsetと予約長の両方を整列する現行契約を維持する。
flush/invalidateはatom全体へのアクセスとして同期判定されるため、隣leaseが同じatomを共有しないことを確認する。
[vkFlushMappedMemoryRanges](https://docs.vulkan.org/refpages/latest/refpages/source/vkFlushMappedMemoryRanges.html)、[vkInvalidateMappedMemoryRanges](https://docs.vulkan.org/refpages/latest/refpages/source/vkInvalidateMappedMemoryRanges.html)

### スレッド、staging、完了後の回収

**VMAの契約:** 既定VMAの内部lockはallocator操作を支えるが、同じ`VmaAllocation`への操作は外部同期する。
`VmaVirtualBlock`も同時利用を直列化する。mapped byte列に対するCPU accessやGPU accessの競合は、
allocatorのlockだけでは解決しない。
[VMA: General considerations](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/general_considerations.html)

Virtual allocatorは一つのbuffer内のoffsetを管理する用途に使えるが、実GPUメモリを確保せず、
GPU完了やreadbackのCPU利用終了も判断しない。確保容量、要求サイズと整列予約サイズ、
空き不足、GPU完了、CPU消費完了をRenderer側で追跡する。
[VMA: Virtual allocator](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/virtual_allocator.html)

**仕様要件:** 参照するsubmitted commandが完了するまでメモリを解放しない。
**本Renderer:** CPUハンドル削除、GPUオブジェクトの破棄、VMA allocationの返却を分け、
記録済み・未投入の命令から必要な所有者も保持する。資源の所有型を保つこととGPU完了の証明を区別する。
[vkFreeMemory](https://docs.vulkan.org/refpages/latest/refpages/source/vkFreeMemory.html)

### Budget・OOM・統計

**一般推奨:** heapの最大容量、process usage、動的budget、VMA block確保量、allocation使用量、
論理resourceサイズ、stagingの予約high-waterを別の値として記録する。
`VK_EXT_memory_budget`のusageは推定、budgetは外部負荷で変化する目安であり、次回の作成成功を予約しない。
[VkPhysicalDeviceMemoryBudgetPropertiesEXT](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceMemoryBudgetPropertiesEXT.html)

budget extensionとVMA用flagを有効にした場合は、利用版の更新手順も守る。
budget超過拒否の`WITHIN_BUDGET`、新block作成拒否の`NEVER_ALLOCATE`、native OOMと、
Rendererの固定staging容量不足を混同しない。VMAの軽量budget照会と詳細statisticsも用途を分ける。
[VMA: Staying within budget](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/staying_within_budget.html)

**本Renderer:** 通常経路の容量不足には待機・再試行の選択を呼び出し側へ返す。
native作成失敗後も既存資源を有効に保つか、Device loss等で停止するかをエラー種別で決め、
無制限なpool増設や全体idleを暗黙のOOM対策にしない。これは現行方針であり、一般仕様の固定回復アルゴリズムではない。

### 後続の最適化: aliasing・defragmentation・GPU address

**後続:** aliasingは、実際のGPU使用が重ならないこと、size/alignment/typeの適合、dedicated条件、
画像作成flags、切り替え時の依存と初期化を満たして導入する。`CAN_ALIAS`は寿命解析や同期を実装しない。
共有allocationの所有者は一つにし、各alias資源の破棄時に同じallocationを重複解放しない。
[VMA: Resource aliasing](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/resource_aliasing.html)

**後続:** defragmentationはVMAの自動透明移動ではない。Rendererが再作成、内容のGPU copy、
旧命令の完了待ち、View/descriptor/address等の参照更新と旧オブジェクトの回収を統合する。
移動できない資源を明示し、コピー量・停止時間と削減したblock量を計測する。
[VMA: Defragmentation](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/defragmentation.html)

**後続:** buffer device addressはVulkan 1.3で対応していても、自動で有効にはならない。
Deviceのfeature有効化、buffer usage、allocationのdevice-address条件と、VMAへの有効化通知をそろえる。
GPU pointerは所有権を保持せず、移動や破棄後の古いaddressを修復してくれない。
[Khronos: Buffer Device Address](https://docs.vulkan.org/guide/latest/buffer_device_address.html)、[VMA: Enabling buffer device address](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/enabling_buffer_device_address.html)

### このリポジトリへの適用

現行のVMA既定pool、bounded execution/staging、atom単位lease、明示完了とreadback消費後の回収を維持する。
M2でusageやformatを増やす際に、transfer-onlyのlayoutと内容有効性の結合を解き、
ImageView・descriptor・pipelineも同じGPU寿命へ含める。aliasing、defrag、BDAは導入時に別途実証する。
この調査は実機non-coherent memory、OOM、Device lossの追加実行確認を意味しない。

<a id="synchronization"></a>

## 同期・キュー・GPU完了

### 順序とメモリ可視性を別々に設計する

**仕様要件:** 同じキュー、同じCommandBuffer、連続したsubmitという条件だけでは、
先行コマンドの書き込みを後続コマンドが正しく読めるとは限らない。
コマンドは明示された依存の範囲でのみ順序と可視性を保証する。
CommandBufferの境界を同期点として扱わない。
[Command Buffers](https://docs.vulkan.org/spec/latest/chapters/cmdbuffers.html)

execution dependencyは処理の実行順を決め、memory dependencyはさらに書き込みの
availabilityと後続アクセスへのvisibilityを扱う。
RAWとWAWには適切なメモリ依存が必要で、WARは実行依存で解決できる。
read同士でも画像レイアウト変更やキュー所有権の条件は別に確認する。
[Synchronization: execution and memory dependencies](https://docs.vulkan.org/spec/latest/chapters/synchronization.html#synchronization-dependencies)

**本リポジトリの適用:** Graph上の「どのproducerの内容を読むか」、
RAW/WAR/WAWによる実行順、native stage/access/layoutへの具体化を分ける。
不要パス除去の生存解析でWAR/WAWをすべてproducer辺として辿ると、
上書き済みのwriterや不要なreaderまで残る。これはGraph設計上の帰結であり、VulkanのAPI規則そのものではない。

### stage・accessは実際の用途から導く

**仕様要件:** access maskはstage maskで対象となるアクセスと整合しなければならない。
例えばvertex bufferの読み込みは通常`VERTEX_ATTRIBUTE_INPUT`であり、
`VERTEX_SHADER`を指定しただけでは代替にならない。
[VkMemoryBarrier2](https://docs.vulkan.org/refpages/latest/refpages/source/VkMemoryBarrier2.html)

以下はSynchronization2での初期対応例。表では`VK_PIPELINE_STAGE_2_`、
`VK_ACCESS_2_`、末尾の`_BIT`を省略する。実際に使うshader stage、usage、
read-modify-write、subresource範囲に応じて追加する。

| GPU用途 | stage | access |
|---|---|---|
| buffer/image copyのsource | `COPY` | `TRANSFER_READ` |
| buffer/image copyのdestination | `COPY` | `TRANSFER_WRITE` |
| compute storage読み込み／書き込み | `COMPUTE_SHADER` | `SHADER_STORAGE_READ`／`SHADER_STORAGE_WRITE` |
| fragmentでのsampled image読み込み | `FRAGMENT_SHADER` | `SHADER_SAMPLED_READ` |
| vertex属性読み込み | `VERTEX_ATTRIBUTE_INPUT` | `VERTEX_ATTRIBUTE_READ` |
| index読み込み | `INDEX_INPUT` | `INDEX_READ` |
| indirect命令の読み込み | `DRAW_INDIRECT` | `INDIRECT_COMMAND_READ` |
| color attachment書き込み | `COLOR_ATTACHMENT_OUTPUT` | `COLOR_ATTACHMENT_WRITE`、既存内容も読む場合は`COLOR_ATTACHMENT_READ`も含む |
| depth/stencil attachment | `EARLY_FRAGMENT_TESTS`と`LATE_FRAGMENT_TESTS`を必要に応じて含む | `DEPTH_STENCIL_ATTACHMENT_READ`／`DEPTH_STENCIL_ATTACHMENT_WRITE` |
| CPU readbackの終点 | `HOST` | `HOST_READ` |

stageの定義は[VkPipelineStageFlagBits2](https://docs.vulkan.org/refpages/latest/refpages/source/VkPipelineStageFlagBits2.html)、
accessの定義は[VkAccessFlagBits2](https://docs.vulkan.org/refpages/latest/refpages/source/VkAccessFlagBits2.html)を参照する。

**一般推奨:** 最初に正しい依存を確立し、その後producerを含むstageまでsourceを絞り、
consumerが必要とするstageまでdestinationを遅らせる。
全域の`ALL_COMMANDS`を使った同期の恒久化は、独立した処理の重なりを減らし得る。
一方、絞り込みで実際のアクセスを除外してはいけない。
性能効果はGPU・負荷依存であり、サンプルの改善率を自分のRendererへ移さない。
[Using pipeline barriers efficiently](https://docs.vulkan.org/samples/latest/samples/performance/pipeline_barriers/README.html)

`VkMemoryBarrier2`で複数リソースの同じ依存をまとめられる。
画像レイアウトや所有権変更には対象を特定したbarrierが必要になる。
globalかresource別かを一律に最適とせず、同じ同期意味を保った上で計画の簡潔さと実機を比較する。
[Synchronization examples](https://docs.vulkan.org/guide/latest/synchronization_examples.html)

### 画像レイアウト・subresource・キュー所有権

**仕様要件:** `VkImageMemoryBarrier2`はmip/layer/aspectの範囲に対して、
メモリ依存、レイアウト遷移、必要ならqueue family所有権移譲を表す。
異なるfamilyでEXCLUSIVE資源を使うときはrelease/acquireとキュー間の実行依存を整合させる。
同じfamilyの異なるキュー間で所有権移譲が不要でも、必要なアクセス依存まで消えるわけではない。
[VkImageMemoryBarrier2](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageMemoryBarrier2.html)

`oldLayout = UNDEFINED`は古い内容を保持しない選択であり、画像をゼロ初期化しない。
有効な内容を次Graphへ引き継ぐimportで、無条件にUNDEFINEDを指定しない。
`GENERAL`も同期を不要にしない。
[VkImageLayout](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageLayout.html)

**本リポジトリの適用:** 内容の初期化済み範囲、layout、最後のアクセス、所有queue family、
投入済み予定状態、実完了状態を別々に管理する。
同じ物理画像やそのViewを再importしても状態を二重化しない。
submitを受理したAの後へBを投入する場合、Bの計画はAの予定された最終状態を引き継ぎ、
CPUアクセスや回収は実際の完了を待つ。この区別を省くと不要なCPU待機か古い状態の使用につながる。

**仕様上の制約:** Vulkan 1.3の通常のDynamic Renderingでは、
`dynamicRenderingLocalRead`等の追加機能を有効にしないまま、
`vkCmdBeginRendering`と`vkCmdEndRendering`の間に任意のpipeline barrierを入れられない。
本基準ではパス間barrierをrendering scopeの外へ置く。
Vulkan 1.4や追加拡張の例をそのまま1.3の基準へ持ち込まない。
[vkCmdPipelineBarrier2: VUID 09553](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdPipelineBarrier2.html)

### Fence・binary・timelineの役割を区別する

| 手段 | 主な用途 | 注意 |
|---|---|---|
| pipeline barrier | 同じキューのアクセス依存 | 別キューを単独では同期しない |
| binary semaphore | キュー間のsignal/wait、WSI | 待機によってunsignaledへ戻る。signal/waitを1対1に管理 |
| timeline semaphore | 値によるキュー／host依存、完了管理 | 値とそのsignal scopeを対応させる。host signalをGPU完了証明と混同しない |
| Fence | キュー仕事の完了をhostで確認 | 次回submitにはunsignaledで、未完了投入と関連していないことが必要 |

binaryとtimelineのpayloadは[VkSemaphoreType](https://docs.vulkan.org/refpages/latest/refpages/source/VkSemaphoreType.html)、
Fenceのsubmit条件は[vkQueueSubmit2](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html)を参照する。

**仕様要件:** Synchronization2のsemaphore signal/waitには`stageMask`によるscopeがある。
signalが対象資源の最後のアクセスを含まず、waitが最初のconsumerを含まない設計では、
その値を資源全体の回収証明にしてはいけない。
semaphoreによるメモリ依存が足りる箇所に同じbarrierを重ねる必要はないが、
layout遷移とqueue family所有権は別に成立させる。
[VkSemaphoreSubmitInfo](https://docs.vulkan.org/refpages/latest/refpages/source/VkSemaphoreSubmitInfo.html)

**一般推奨:** timelineは単調増加する値と投入単位の対応を明示し、
未投入signalへのwaitや循環依存で進行不能にならないよう設計する。
異なるキューの数値を単純な最大値にまとめて全資源の完了と見なさない。
本実装のFence方式を置き換えること自体は性能改善の証明にならない。
[Timeline semaphore sample](https://docs.vulkan.org/samples/latest/samples/extensions/timeline_semaphore/README.html)

timelineの未完了wait/signal値と現在値の差はDeviceの上限に従う。
[VkPhysicalDeviceTimelineSemaphoreProperties](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceTimelineSemaphoreProperties.html)

### hostアクセスとGPU完了

**仕様要件:** host writeがsubmitより前にhost memory model上で確立していれば、
queue submissionがhost→deviceの必要なdomain operationを行う。
一方、Fence待機だけではGPU書き込みのhost可視性は保証しない。
readbackにはdevice→`HOST_READ`のメモリ依存を設け、非coherent時は完了後にinvalidateする。
coherentなメモリでもGPU実行とCPUアクセスの競合は自動解決されない。
[Synchronization: host write ordering and fence scope](https://docs.vulkan.org/spec/latest/chapters/synchronization.html)

**本リポジトリの適用:** CPUのstaging書き込み終了、必要なflush、submit、
実GPU完了、readback invalidate、CPUコピー終了を別の境界として扱う。
Upload領域とReadback領域の回収条件も区別する。
すべての転送の直後にCPU wait/readbackを強制せず、呼び出し元が必要とする完了境界だけを待つ。

### CommandPool・CommandBufferとhost並列性

**仕様要件:** pendingなCommandBufferをreset/freeしてはいけない。
pool reset時は配下のすべてのCommandBufferがpendingでないことが必要であり、
poolをhostから同時操作しない。Fence resetも未完了のqueue commandに関連する間は禁止される。
resetの成功を確認できた組だけを次の実行へ貸し出す。
[vkResetCommandPool](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetCommandPool.html)、
[vkResetFences](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetFences.html)

**一般推奨:** 同時に回収できる実行単位ごとにpoolを再利用し、
必要な場合に限って個別reset用の`RESET_COMMAND_BUFFER`を選ぶ。
並列記録ではthreadと未完了実行単位ごとにpoolの所有を分ける。
secondaryを極端に細かく分けると記録・実行の管理負担が増えるため、
draw数、CPU並列度、command sizeを測定する。
[Command buffer usage and multi-threaded recording](https://docs.vulkan.org/samples/latest/samples/performance/command_buffer_usage/README.html)

**仕様要件:** 外部同期が要求されるhostオブジェクトはアプリケーション側で排他する。
CommandBuffer操作は元のCommandPoolの同期も伴う。
Rustの共有所有や型の`Send`/`Sync`だけで個々のVulkan呼び出し条件を満たしたとは判断しない。
[Threading behavior](https://docs.vulkan.org/spec/latest/chapters/fundamentals.html#fundamentals-threadingbehavior)

**本リポジトリの適用:** Vulkan 1.3基準では共有Queueのsubmit/present/idle等を
一つのhost同期方針へまとめる。異なるキューへの投入は独立に行えるが、
VkQueueと実ハードウェアの対応は実装依存で、専用transfer/compute queueの追加だけでは高速化を保証しない。
[Queues](https://docs.vulkan.org/guide/latest/queues.html)

### 破棄・submit失敗・Device loss

**仕様要件:** bufferと、それを通じて参照するmemoryは、関連する投入済みコマンドの
実行中に破棄・解放しない。View経由の参照も含む。
**本リポジトリの適用:** 記録からsubmitまでにも所有を保持し、
cancel、正常完了、timeout、Device停止を区別したretirementへ渡す。
CPUの最後のハンドルが消えた時点をnative解放時点にしない。
[vkDestroyBuffer](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroyBuffer.html)、
[vkFreeMemory](https://docs.vulkan.org/refpages/latest/refpages/source/vkFreeMemory.html)

**仕様要件:** `vkQueueSubmit2`がhost/device OOMを返す場合は、参照資源と同期状態を
変更しない保証がある。この保証ができない投入失敗は`VK_ERROR_DEVICE_LOST`となる。
**本リポジトリの適用:** 未投入を証明できる失敗だけをrollbackし、
不明な状態を成功済み・初期化済みとして公開しない。
[vkQueueSubmit2](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html)

`VK_TIMEOUT`は未完了であり、Fenceの破棄や資源再利用を許可しない。
waitのtimeout精度は実装依存で、指定時間を壁時計の厳密な終了上限として約束しない。
[vkWaitForFences](https://docs.vulkan.org/refpages/latest/refpages/source/vkWaitForFences.html)

**仕様要件:** Device loss後の資源内容は正常な出力として扱えず、子オブジェクトも自動破棄されない。
lossを返すAPIの`VK_ERROR_DEVICE_LOST`は、pending/in-useの判定に限って成功と同等に扱う規則がある。
これは結果の正しさやDeviceの復帰を意味しない。
**本リポジトリの適用:** Deviceを終端状態へ移し、新規仕事を止め、
各待機結果と保持資源を整理して破棄する。未確認timeoutに対する保守的な保持はプロジェクトの安全方針であり、
仕様が確認済みDevice lossのすべてに永久リークを要求するわけではない。
[Lost Device](https://docs.vulkan.org/spec/latest/chapters/devsandqueues.html#devsandqueues-lost-device)

<a id="shader"></a>

## Shader・SPIR-V・CPU/GPU ABI


### 再現可能な入力と実行環境

- **仕様要件:** SPIR-Vのversion、capability、extension、entry point、stageを、使用DeviceのAPI・有効feature・extensionへ照合する。新しいSDKでコンパイルできたことだけでは、Vulkan 1.3の実行環境で使用可能とは判断しない。[SPIR-V拡張の使用手順](https://docs.vulkan.org/guide/latest/spirv_extensions.html)、[ShaderModuleのValid Usage](https://docs.vulkan.org/refpages/latest/refpages/source/VkShaderModuleCreateInfo.html)
- **本Rendererの初期方針:** 固定toolchain、ソースとinclude依存、マクロ、entry point、stage、最適化・debug設定、target environmentを生成条件へ含める。`glslc --target-env=vulkan1.3`で生成し、同じ環境の`spirv-val`を生成・変換の後に実行する。実行手順は既存のtoolingガイドを維持する。[glslcのオプション](https://github.com/google/shaderc/blob/main/glslc/README.asciidoc)、[SPIRV-Tools](https://github.com/KhronosGroup/SPIRV-Tools)
- **仕様要件:** `VkShaderModuleCreateInfo`へ非ゼロ・4の倍数のSPIR-V byte数と、有効な32-bit word列を渡す。任意のbyte sliceをアラインメント確認なしで`u32`ポインターへcastしない。読み込み・形式・検証失敗はpipeline作成前に原因を付けて返す。[ShaderModuleの入力契約](https://docs.vulkan.org/refpages/latest/refpages/source/VkShaderModuleCreateInfo.html)
- **一般推奨:** GLSL/HLSLの選択とAPI間のbinding・座標変換は別に決める。HLSL→SPIR-Vを選んでも、DX側のdata layoutやregister番号が自動的に既存Graph契約と一致するとは仮定しない。[HLSL in Vulkan](https://docs.vulkan.org/guide/latest/hlsl.html)

### データ配置を明示する

`#[repr(C)]`だけではshaderのarray stride、matrix stride、paddingまで一致する保証はない。
set/binding、descriptor種別・個数・stage、member offset、array/matrix stride、row/column-major、サイズとalignmentを、明示metadataまたはreflectionから照合する。
Rust側のサイズ・offsetの検査と、小さなGPU round-tripでABIを確認する。
これはshader interfaceの要求をホストの型へ具体化する、本Renderer側の実装方針である。[Shader Interfaces](https://docs.vulkan.org/spec/latest/chapters/interfaces.html)

| 配置 | 初期利用時の確認 |
|---|---|
| `std140`相当 | UBOのextended alignment、配列や構造体のpaddingを確認する |
| `std430`相当 | SSBO/push constantsとUBOを混同しない。UBOの標準レイアウトには`uniformBufferStandardLayout`の有効化が必要 |
| scalar layout | `scalarBlockLayout`を照会・有効化する。Workgroupのscalar layoutには別のfeature契約がある |
| relaxed layout | Vulkan 1.1でcore化され、独立feature bitはない。scalar layout全体と同じ契約ではない |

core化した版だけでfeatureが有効になったとは扱わず、実際のfeature chainと生成SPIR-Vを照合する。
検証ツールのlayout緩和オプションは、有効にしたDevice契約と対応させる。検証を通すためだけに緩和しない。
[Shader Memory Layout](https://docs.vulkan.org/guide/latest/shader_memory_layout.html)

### 小さな定数とbuffer binding

- **仕様要件:** Push constantはpipeline layoutのrange・stage・互換性、4-byte単位のoffset/size、Deviceの`maxPushConstantsSize`を満たす。command buffer記録開始時の内容は未定義であり、読まれるbyteは使用前に設定する。[vkCmdPushConstants](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdPushConstants.html)
- **一般推奨:** 小さなper-draw定数にはpush constantを候補にし、大きい配列や頻繁な更新にはUBO/SSBOとdynamic offsetを比較する。全GPU・全データ量でpush constantが最速とは保証しない。[Push Constants](https://docs.vulkan.org/guide/latest/push_constants.html)、[NVIDIAのpipeline layout指針](https://developer.nvidia.com/blog/vulkan-dos-donts/)
- **仕様要件:** descriptor buffer range、dynamic offsetとbase offsetの合計、`minUniformBufferOffsetAlignment`/`minStorageBufferOffsetAlignment`、最大rangeを別々に検証する。巨大な共有bufferに対する`VK_WHOLE_SIZE`がdescriptor種別の最大range内とは限らない。[VkDescriptorBufferInfo](https://docs.vulkan.org/refpages/latest/refpages/source/VkDescriptorBufferInfo.html)、[Device limits](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceLimits.html)

<a id="bindings"></a>

## Descriptor・pipeline・キャッシュ

### Descriptorは参照先の寿命を保持しない

**仕様要件:** descriptorの内容と、その参照先buffer/image/view/samplerの所有・同期を別に管理する。
通常のsetは安全な更新点で更新し、記録済み・未完了命令が参照する資源をCPU上の更新・削除から保護する。
単に同じdescriptor handleを再利用できることと、参照先byteを上書きできることは別である。
[Descriptor Sets](https://docs.vulkan.org/spec/latest/chapters/descriptorsets.html)

通常のbindingを`vkUpdateDescriptorSets`で変更すると、GPU pendingとの競合だけでなく、
そのsetを使用するrecording/executable状態のcommand bufferがinvalidになる場合がある。
「submit前だから自由に更新できる」とはしない。
[vkUpdateDescriptorSets](https://docs.vulkan.org/refpages/latest/refpages/source/vkUpdateDescriptorSets.html)

**仕様要件:** descriptor poolのresetは、そこから割り当てたsetの全使用が完了してから行う。
resetはsetを暗黙にfreeするため、poolをframe番号だけで回収せず、完了条件へ結び付ける。
pool/setへのhostアクセスの外部同期も守る。[vkResetDescriptorPool](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetDescriptorPool.html)

**一般推奨:** 同じbinding内容のset/layoutはキャッシュし、変更頻度の近いデータをまとめる。
多数の小buffer・毎drawのallocate/updateを、共有bufferとrange/dynamic offsetへまとめる候補を測定する。
長寿命キャッシュと完了後に一括resetする一時poolは、それぞれの寿命へ合わせる。
キャッシュkeyに参照先の世代と有効性を含め、容量・破棄方針を明示する。
Khronosのmobile測定値をこのPCの改善倍率へ置き換えない。
[Descriptor and buffer management](https://docs.vulkan.org/samples/latest/samples/performance/descriptor_management/README.html)

### Update-after-bind・bindlessは別の契約

**仕様要件:** update-after-bindにはdescriptor種別に対応するfeature、binding flag、set-layout flag、pool flagを揃える。
これによって更新時点の制約が変わっても、実行中のshaderがアクセスするdescriptorを無同期で書き換えてよいとはならない。
`UPDATE_UNUSED_WHILE_PENDING`、partially bound、variable count、non-uniform indexingは、それぞれ別のfeature/flag・使用条件を持つ。
[Descriptor indexing Guide](https://docs.vulkan.org/guide/latest/extensions/VK_EXT_descriptor_indexing.html)、[Descriptor indexing sample](https://docs.vulkan.org/samples/latest/samples/extensions/descriptor_indexing/README.html)

**後続:** bindless tableを導入するときは、shaderが到達し得る集合、table更新の公開時点、slotの世代と回収、参照先のGPU寿命をGraph/runtimeへ組み込む。
M2では通常のdescriptor setから始める。これは複雑さを限定する本Rendererの判断であり、bindless一般の性能評価ではない。

**後続:** `VK_EXT_descriptor_buffer`はlayoutのsize/binding offset、descriptor size、alignmentを実照会して利用する。
単純な`descriptorSize * count`だけでlayout全体の必要byte数を推定しない。
CPU/GPUの書き込み→descriptor読み込みの同期と参照先寿命が必要で、descriptor poolをなくしてもGPU完了管理はなくならない。
core 1.3に一律にある機能として扱わない。[Descriptor Buffer Guide](https://docs.vulkan.org/guide/latest/descriptor_buffer.html)、[Descriptor Buffers仕様](https://docs.vulkan.org/spec/latest/chapters/descriptorbuffers.html)

### Pipelineは作って保持し、作成時間も測る

**一般推奨:** pipeline、layout、sampler、ImageViewを互換条件で再利用し、毎draw/毎frameの再作成を基本動作にしない。
pipelineのアプリ側keyにshader内容、entry point、specialization値、layout、attachment format/sample数と固定・dynamic状態を含める。
初回作成のCPU時間、cache hit、ウォームアップ、実行GPU時間を分けて記録する。
shaderのSPIR-Vを事前生成しても、driver側のpipelineコンパイルがなくなるわけではない。
[Pipeline Management](https://docs.vulkan.org/samples/latest/samples/performance/pipeline_cache/README.html)

**一般推奨:** 必要pipelineを事前ウォームアップするか、対応するworkerで作成してフレーム中の停止を避ける。
作成中・使用中・置き換え待ちの状態を区別し、hot reloadで旧pipelineをGPU完了前に破棄しない。
非同期作成、bind回数の抑制、derivativeの効果はGPU/driver依存で測る。
[NVIDIA Vulkan Dos and Don'ts](https://developer.nvidia.com/blog/vulkan-dos-donts/)

**仕様要件:** 複数pipeline作成は、戻り値が失敗でも一部のpipelineが作成済みの場合がある。
返された非NULL handleを所有して後始末し、失敗値だけを見て配列全体を放棄しない。
[Multiple Pipeline Creation](https://docs.vulkan.org/spec/latest/chapters/pipelines.html#pipelines-multiple)

| 対象 | 破棄・再利用で守ること |
|---|---|
| `VkShaderModule` | pipeline作成への入力。作成したpipelineが使用中でもmoduleを破棄できる。全native objectをGPU完了まで一律保持する必要はない |
| `VkPipeline` | 参照する投入済み命令が完了するまで破棄しない。未submitの記録を再利用する場合も、その命令が有効かを管理する |
| descriptor pool/set | pool/setの使用完了、参照資源の寿命、command bufferの有効性を満たして回収する |

[vkDestroyShaderModule](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroyShaderModule.html)、[vkDestroyPipeline](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroyPipeline.html)、[vkResetDescriptorPool](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetDescriptorPool.html)

### 永続pipeline cacheと作成時の並列性

**仕様要件:** `VkPipelineCache`の永続データには`vkGetPipelineCacheData`で取得した有効なbyte列を使用する。
互換性の判定にはheader version、vendor/device ID、`pipelineCacheUUID`が関わる。異なる環境のcacheを万能なshader binaryとして配布しない。
[VkPipelineCacheCreateInfo](https://docs.vulkan.org/refpages/latest/refpages/source/VkPipelineCacheCreateInfo.html)、[cache header](https://docs.vulkan.org/refpages/latest/refpages/source/VkPipelineCacheHeaderVersionOne.html)

**本Rendererの方針:** ファイルの切断・破損・自前format不一致を検出したら空cacheへ戻せるようにし、
cacheがないことを実行不能の理由にしない。

**仕様要件:** cacheのhost同期はAPI/flagごとの規則を確認する。
`EXTERNALLY_SYNCHRONIZED`を選ぶ場合は対応featureを有効にし、同じcacheの変更をアプリ側で同期する。
「Vulkan objectはすべて単一mutex」「cacheはいつでも自由に並列変更」のどちらも一律の規則にしない。
[cache作成flag](https://docs.vulkan.org/refpages/latest/refpages/source/VkPipelineCacheCreateFlagBits.html)、[vkCreateGraphicsPipelinesのhost同期](https://docs.vulkan.org/refpages/latest/refpages/source/vkCreateGraphicsPipelines.html)

**測定候補:** specialization constantはpipeline作成時に決まる値を最適化へ使えるが、値の組み合わせだけpipeline数と作成/保存コストが増える。
頻繁に変わる値まで全てspecializeせず、GPU時間とcache規模の両方で判断する。
dynamic stateもpipeline数を抑える候補だが、対応feature・使用前の設定・static/dynamic切替時の有効性を確認し、常に高速とは仮定しない。
[Specialization Constants sample](https://docs.vulkan.org/samples/latest/samples/performance/specialization_constants/README.html)、[Pipeline Dynamic State](https://docs.vulkan.org/guide/latest/dynamic_state.html)

<a id="compute"></a>

## Compute・subgroup・shaderの信頼境界

**仕様要件:** dispatchのworkgroup数とshaderのlocal sizeを分けて検証する。
各次元の`maxComputeWorkGroupSize`、積の`maxComputeWorkGroupInvocations`、shared memory、dispatch数の上限をそれぞれ満たす。
[Compute Shaders](https://docs.vulkan.org/guide/latest/compute_shaders.html)、[vkCmdDispatch](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdDispatch.html)

**本Renderer:** 端数をceil-divしてdispatchする場合は、shaderで有効pixel/element範囲を確認するなどして、
範囲外invocationを処理対象にしない。これはこのRendererの結果の正しさに必要な方針であり、
すべての`vkCmdDispatch`が明示的なshader guardを要求するというAPI規則ではない。

**仕様要件:** shader内の実行同期、memory scope、storage classと、API側のdispatch間同期を区別する。
workgroupの`barrier()`は同じworkgroup内のshared memory共有に使うもので、dispatch全体や別dispatchの終了待ちにはならない。
異なるworkgroupに通常のglobal barrierがあると仮定せず、依存する処理は別dispatchへ分けてGraphのbarrierでつなぐ。
barrierへ到達しないinvocationが生じる分岐も、使用する命令の制約に従う。
[GL_KHR_memory_scope_semantics](https://docs.vulkan.org/glslext/latest/glslext/khr/GL_KHR_memory_scope_semantics.html)、[Shaders仕様](https://docs.vulkan.org/spec/latest/chapters/shaders.html)

**測定候補:** local size、memory coalescing、shared memory利用、register圧力、分岐、演算精度を対象GPUで比較する。
shared memoryや大きいworkgroupを増やすほど高速になるとは限らない。
subgroup sizeを32/64に固定せず、supported stage/operationとsize制御featureを照会する。
subgroupとworkgroupの境界を同一視せず、不活性laneや部分subgroupを考慮する。
[Subgroups](https://docs.vulkan.org/guide/latest/subgroups.html)、[HLSLの性能上の検討事項](https://docs.vulkan.org/guide/latest/hlsl.html)

**本Rendererの信頼境界:** `spirv-val`の成功、Graphのbinding/range検査、robustnessの有効化は、任意の未信頼shaderの完全なサンドボックスとは扱わない。
`robustBufferAccess`、imageに対する保証、robustness2などの範囲は異なり、raceや誤った同期・アルゴリズムまで修正しない。
本Rendererの初期shaderは管理下の信頼されたassetに限定し、out-of-boundsを生成段階・CPU検査・GPU内容照合でも防ぐ。
robustnessの選択は安全要件と測定したコストに基づき、単に検証を静かにする目的で無効化しない。
[Robustness](https://docs.vulkan.org/guide/latest/robustness.html)

<a id="raster"></a>

## 描画・画像・Attachment（M2で導入）


### Dynamic Renderingと状態管理

- **仕様要件:** `vkCmdBeginRendering`は利用する画像を指定されたlayoutへ遷移させない。画像はアクセス時点で`imageLayout`と一致している必要があり、前後の依存とlayout遷移はGraphの計画から明示する。Dynamic RenderingによるRenderPass／Framebuffer作成の削減と、同期が不要になることを混同しない。[vkCmdBeginRendering](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdBeginRendering.html)
- **仕様要件:** `VkPipelineRenderingCreateInfo`が色・深度・stencil attachment形式とview maskを定める。描画時の条件に適合するpipelineを選び、互換性に関係する値をpipeline識別へ含める。[VkPipelineRenderingCreateInfo](https://docs.vulkan.org/refpages/latest/refpages/source/VkPipelineRenderingCreateInfo.html)
- **本Rendererへの適用:** resourceの識別、layout、アクセス、内容の有効範囲、投入済みの予定状態、実完了状態を別々に管理する。`TRANSFER_SRC_OPTIMAL`等のlayoutを「有効な画像内容がある」というフラグへ流用しない。`UNDEFINED`を使って旧内容を捨てても、同じメモリを使う先行処理との競合まで消えるわけではない。[Synchronization: image layout transitions](https://docs.vulkan.org/spec/latest/chapters/synchronization.html#synchronization-image-layout-transitions)

### clear／load／storeと内容の有効性

| 宣言 | Graphで扱う意味 |
|---|---|
| `LOAD` | render area内の既存内容を読む。必要な範囲が有効でなければエラー |
| `CLEAR` | render area内を指定値で初期化する。描画していない画素の値も定められる |
| loadの`DONT_CARE` | 以前の内容を保持しない。後で読む画素はclearまたは描画等で確立する |
| `STORE` | render area内で生成した内容を後続用途へ残す |
| storeの`DONT_CARE` | render area内の内容は後続の入力として使えない |

これはrender area内の規則であり、部分領域だけのclearを画像全体の初期化として扱わない。
深度・stencilのaspect、mip、layerにも同じ粒度の契約が必要である。
`DONT_CARE`にはattachment writeのアクセスがあるため、「何もしない」として同期から削除しない。
`NONE`との違いは使用するAPI版の規則を確認する。storeの`NONE`はcore 1.3、loadの`NONE`はcore 1.4または対応する`load_store_op_none`拡張であり、1.3だけを理由にloadの`NONE`を使わない。
[VkAttachmentLoadOp](https://docs.vulkan.org/refpages/latest/refpages/source/VkAttachmentLoadOp.html)、[VkAttachmentStoreOp](https://docs.vulkan.org/refpages/latest/refpages/source/VkAttachmentStoreOp.html)

**一般推奨:** 初期値が必要ならattachmentのload clearを使い、既存内容が必要なときだけloadを選ぶ。
後続のconsumerが使わないattachmentをstoreしない。clearを毎回のfullscreen shaderや独立clear命令に置き換える前に用途を確認する。
ただし部分clear等には独立命令が必要な場合がある。Khronosの帯域削減例は主にtile-based GPUでの測定であり、全GPUに同じ改善率を保証しない。
[Appropriate use of render pass attachments](https://docs.vulkan.org/samples/latest/samples/performance/render_passes/README.html)

### 形式・深度・MSAA・View

- **仕様要件:** format名だけで全用途の対応を決めない。color/depth attachment、storage、sampled、filter、transferの必要条件を照会し、実際のformat/type/tiling/usage/flagsの組み合わせとsample数、mip・layer・extent上限を検証する。[Formats](https://docs.vulkan.org/spec/latest/chapters/formats.html)、[VkImageFormatProperties](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageFormatProperties.html)
- **一般推奨:** 深度形式は用途を基準に選ぶ。depth/stencil attachment対応とsampling／blit対応は別の条件である。depthアクセスの依存は必要なEARLY／LATE_FRAGMENT_TESTSとDEPTH_STENCIL_ATTACHMENT_READ／WRITEへ対応づける。片方だけに絞れる根拠がなければ両方のstageを含める。[Depth](https://docs.vulkan.org/guide/latest/depth.html)
- **本Rendererへの適用:** 通常Z／reversed-Z、clip/depth範囲、clear値、compare op、Y方向、front face、行列規約を一組で決め、既知の重なった形状で確認する。reversed-Zは画質上の選択肢であり、未測定の速度改善として扱わない。Vulkan標準のclip深度は`0 <= Zc <= Wc`を基準とする。[Depth: primitive clipping](https://docs.vulkan.org/guide/latest/depth.html#primitive-clipping)
- **仕様要件:** ImageViewは画像のsubresource範囲と形式解釈を指定する。view usageはimage usageを越えられず、format reinterpretationにも互換性と作成flagsの条件がある。[VkImageViewCreateInfo](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageViewCreateInfo.html)
- **推奨／寿命:** 同一条件のViewは保持・再利用し、論理Viewを別resourceとして扱ってhazardを隠さない。提出済み命令の参照が残るViewを破棄しない。Viewのcacheキーには物理画像の世代、範囲、形式、type等を含める。このcache設計自体は本Rendererの実装判断である。[vkDestroyImageView](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroyImageView.html)
- **仕様／推奨:** multisample画像の縮小は単純blitではなくresolveの契約で扱う。resolve形式・sample数・modeの条件を検証する。特にdepth/stencil resolveの対応modeを色と同一視しない。[VkRenderingAttachmentInfo](https://docs.vulkan.org/refpages/latest/refpages/source/VkRenderingAttachmentInfo.html)、[vkCmdBlitImage](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdBlitImage.html)
- **条件付き最適化:** MSAA結果だけを後続で使うなら、未resolveのattachmentを外部メモリへstoreしない構成を検討する。tile上resolveとtransient memoryの利益は対象GPUで計測する。[MSAA](https://docs.vulkan.org/samples/latest/samples/performance/msaa/README.html)

### tile memoryと描画パスの統合

`TRANSIENT_ATTACHMENT`と`LAZILY_ALLOCATED`は、必要なusage／memory typeが対応し、内容を後の通常sampling・storage・transferへ残す必要がない場合に検討する。
すべての一時画像へ機械的に指定しない。M2の「描画結果を別Computeパスで読む」画像はそのconsumerまで内容を保存する必要がある。
サブパスが同じtileへ統合されるかはattachment容量、依存、GPU実装等に依存し、論理的なパス統合だけで保証されない。
[Subpasses](https://docs.vulkan.org/samples/latest/samples/performance/subpasses/README.html)

Dynamic Renderingでも`VK_KHR_dynamic_rendering_local_read`を使うとpixel-local inputを扱えるが、1.3の`dynamicRendering`だけからは利用可能と判断しない。
任意座標のtexture samplingとpixel-local readの用途は異なる。M2の単純な別パス構成を基準として、必要になった時点でsubpass／local readと外部帯域・GPU時間を比較する。
[Dynamic Rendering local read](https://docs.vulkan.org/samples/latest/samples/extensions/dynamic_rendering_local_read/README.html)

画像の既知layoutを保持し、不要な`UNDEFINED`経由の遷移を避けることには実装依存の利益もある。
Khronosのlayout sampleはMaliのtransaction eliminationを説明するもので、「blitよりshaderが常に速い」等へ一般化しない。
必要なdiscardと内容を捨てずに行うtransitionを区別し、最終的には対象GPUのcounterで判断する。
[Layout transitions](https://docs.vulkan.org/samples/latest/samples/performance/layout_transitions/README.html)

### 画像転送・mip・色と出力

**仕様要件:** copyはformat解釈の変換器ではない。texel block、offset／extent、aspect、layer等のcopy条件を守る。
compressed／multi-planar形式をRGBA8の`width * height * 4`へ一般化しない。mipを生成・転送する場合は各subresourceの初期化とアクセスを追跡する。
[Image Copies](https://docs.vulkan.org/guide/latest/image_copies.html)、[vkCmdCopyImage](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdCopyImage.html)

**仕様／推奨:** scalingやfilter・format変換を要するblitは対応format featureを確認する。linear blitできない形式に別経路が必要な場合は、明示的に実装または未対応を返す。
数値・normal等のdata textureと色textureのmip生成を同一の色変換規則で処理しない。
[vkCmdBlitImage](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdBlitImage.html)

**本Rendererへの適用:** storage format、shaderが扱うlinear値、textureのsRGB decode、attachment writeのencode、Swapchainの`VkColorSpaceKHR`を区別する。
sRGB attachmentへのshader出力をさらに手動sRGB encodeして二重変換しない。
HDRではformatだけで表示が決まらず、色域・transfer function・tone mapping・表示先条件を契約化する。alphaの扱いも別に決める。
[FramebufferのsRGB変換](https://docs.vulkan.org/spec/latest/chapters/framebuffer.html)、
[VkColorSpaceKHR](https://docs.vulkan.org/refpages/latest/refpages/source/VkColorSpaceKHR.html)

<a id="wsi"></a>

## WSI・Swapchain（M3で導入）

### Surface能力と待機の境界

**仕様要件:** Graphics＋Compute対応のqueueが、対象Surfaceへのpresentに対応するとは限らない。
実Surfaceに対するpresent対応、formatとcolor spaceの組、present mode、usage、transform、composite alpha、extentを照会する。
異なるqueue familyを使う場合はsharing／ownershipとSemaphore依存を設計し、present命令自体がfamily ownershipを移すと仮定しない。
[vkQueuePresentKHR](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueuePresentKHR.html)、[WSI specification](https://docs.vulkan.org/spec/latest/chapters/VK_KHR_surface/wsi.html)

`minImageCount`は要求する最低数であり、作成後は実際の画像数を取得して配列を構成する。
Surfaceの`maxImageCount == 0`は上限なしを表し、`min(request, maxImageCount)`で0画像にしない。
可変extentと固定`currentExtent`を区別し、最小化等で有効extentがゼロなら作成・描画を保留する。
[VkSurfaceCapabilitiesKHR](https://docs.vulkan.org/refpages/latest/refpages/source/VkSurfaceCapabilitiesKHR.html)、[Swap chain](https://docs.vulkan.org/tutorial/latest/03_Drawing_a_triangle/01_Presentation/01_Swap_chain.html)

**条件付き最適化:** FIFO、MAILBOX、画像数、CPUのframes in flightはthroughput、待ち時間、入力latency、消費電力に異なる影響を持つ。
実際のpresent mode対応と目標を調べ、常に3画像／MAILBOXを最適と固定しない。
Khronos sampleのmobile測定値をPCの性能予測へ使わない。
[Choosing the right number of swapchain images](https://docs.vulkan.org/samples/latest/samples/performance/swapchain_images/README.html)

### acquireとpresentのSemaphore

**仕様要件:** acquireに渡すSemaphoreはbinaryで、未signalかつ未完了のsignal／waitがないものを使う。
presentが待つSemaphoreもbinaryであり、対応するsignalを提出済みにする必要がある。
timeline Semaphoreをランタイム内部で採用しても、このWSI境界のbinary条件は消えない。
acquireのCPU returnと、acquire同期対象がGPU側から利用可能になる時点を区別する。
[vkAcquireNextImageKHR](https://docs.vulkan.org/refpages/latest/refpages/source/vkAcquireNextImageKHR.html)、[vkQueuePresentKHR](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueuePresentKHR.html)

**一般推奨:** frame slotのcommand buffer／Fence／acquire Semaphoreと、Swapchain imageのindexを別概念として持つ。
拡張を使わない基本構成ではpresent待機SemaphoreをSwapchain画像ごとに持ち、同じ画像を再acquireしてそのacquire同期を満たしたことを再利用の根拠とする。
描画submitのFence完了だけでは、presentation側によるそのSemaphoreの使用終了を証明できない。
[Swapchain Semaphore Reuse](https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html)

### 再作成・異常終了・present資源の回収

**本Rendererの状態機械:** `SUCCESS`、画像取得を伴う`SUBOPTIMAL`、未取得の`TIMEOUT`／`NOT_READY`、`OUT_OF_DATE`、`SURFACE_LOST`、`DEVICE_LOST`を別々に分類する。
`SUBOPTIMAL`を一律の未取得失敗にすると、既に取得した画像やsignal予定の同期を取り残し得る。
acquire後にsubmitを行わず終了する経路も設計し、次のsubmitを確定する前にframe Fenceをresetして永久待機を生まない。
これらの状態機械は本Rendererで実装・試験する契約であり、現時点で確認済みのWSI実装ではない。
[vkAcquireNextImageKHR return codes](https://docs.vulkan.org/refpages/latest/refpages/source/vkAcquireNextImageKHR.html)、[Swapchain recreation tutorial](https://docs.vulkan.org/tutorial/latest/03_Drawing_a_triangle/04_Swap_chain_recreation.html)

**仕様要件:** presentの`OUT_OF_DATE`や`SURFACE_LOST`等は、Semaphore waitを含むqueue操作が既にenqueueされたうえで返る場合がある。
一方、enqueue失敗時のhost/device OOMには状態不変の保証がある。返値をすべて「未投入だから元へ戻せる」と処理しない。
[vkQueuePresentKHR error semantics](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueuePresentKHR.html)

**一般推奨:** old Swapchain、present待機Semaphore、View等の世代を保持し、通常の描画Fenceだけを根拠に旧世代を解放しない。
`oldSwapchain`を指定して新規作成しても、旧世代の使用が即時終了するわけではない。
拡張なしの回収方法はKhronos sampleが示すpresent履歴・再acquire等の根拠を確認する。連続リサイズ中の旧世代の蓄積には上限と待機・停止方針が必要になる。
[Swapchain Recreation](https://docs.vulkan.org/samples/latest/samples/api/swapchain_recreation/README.html)

**拡張が使える場合:** `VK_KHR_swapchain_maintenance1`／対応するEXT版のpresent Fenceは、対象資源のpayloadをpresentation engineが参照取得したこと等を証明し、SemaphoreやSwapchainを安全に解放する根拠を提供する。
このFenceは画面への表示完了を待つとは限らない。`VK_KHR_present_wait`のpresent待機は別の目的・対応条件を持つ。
Swapchainの破棄には、そのSwapchainの過去のpresent要求に対応する全Fenceのsignalを確認する。
present待機Semaphoreを破棄する場合は、そのSemaphoreに対応するpresent Fenceの条件に加えて、
描画等からの通常の参照も終了していることを確認する。一件のpresent完了をSwapchain全体の回収証明にしない。
これらをVulkan 1.3の必須core機能として扱わず、拡張・feature・依存と使用中のSDK/APIを確認する。
[VkSwapchainPresentFenceInfoKHR](https://docs.vulkan.org/refpages/latest/refpages/source/VkSwapchainPresentFenceInfoKHR.html)、[vkWaitForPresentKHR](https://docs.vulkan.org/refpages/latest/refpages/source/vkWaitForPresentKHR.html)

拡張なしの`vkDeviceWaitIdle`／`vkQueueWaitIdle`をpresent資源の完了証明として一般化しない。
通常のGPU処理の完了とpresentation固有の使用終了は別条件であり、shutdownも対象にした回収方法を選ぶ。
[Swapchain Semaphore Reuse: maintenance1](https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html#_vk_ext_swapchain_maintenance1_extension)

### 導入時の受入条件

- M2: clear/load/storeと部分有効範囲、depth occlusion、attachment→shader→copy、宣言とView／formatの整合、再実行時のView／pipeline保持をCPU・実GPUで分けて確認する。
- M3: acquire／present成功だけでなく、最小化、急速なリサイズ、`OUT_OF_DATE`、取得後キャンセル、終了、画像数変更、frame slotとimage indexの違いを検証する。
- present Semaphoreの再signal、旧世代の早期解放、false completionがないことをValidationと明示的な状態試験で確認し、未対応拡張の経路も区別する。

<a id="measurement"></a>

## 検証・診断・性能測定

調査日: 2026-11-12。以下は一次資料から導いた実装指針であり、新しい実GPU試験の結果ではない。
このリポジトリのAPI baselineはVulkan 1.3、固定SDKは1.4.341.1である。
`latest`の文書に追加された検証機能・設定・GPU世代を、固定SDKや現在の実装が備えるとは扱わない。

### Validationを目的別に使う

VulkanのValid Usage違反は未定義動作を招く。ドライバーの暗黙の入力検査へ依存せず、
開発初期から`VK_LAYER_KHRONOS_validation`を使い、報告されたVUIDと該当仕様を確認する。
一台で見かけ上動いたことは移植性の根拠にならない。
[Khronos: Validation Overview](https://docs.vulkan.org/guide/latest/validation_overview.html)

| 目的 | 使用する検査 | 本リポジトリへの適用 |
|---|---|---|
| API・オブジェクト・通常の状態 | Core validation | 正しさ受入の既定経路 |
| コマンド間のアクセス競合 | Synchronization Validation | Coreを残して追加する |
| シェーダー実行時にしか分からないアクセス | GPU-Assisted Validation | M2のshader導入後、追加の明示実行経路として検討 |
| 仕様違反に限らない疑わしい使用・性能傾向 | Best Practices Validation | 定期的な追加診断。各warningの対象機材と根拠を確認 |

SyncValの検査対象には制限がある。固定SDKの資料ではhost memory access、memory aliasing、
shaderが実際に触ったdescriptorの厳密な集合、間接参照される一部bufferなどを完全には追跡しない。
検証ゼロでも、CPU/GPU競合・寿命・aliasing・Graph宣言漏れの保証は別途必要である。
[LunarG: SDK 1.4.341.1 SyncVal](https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/synchronization_usage.html)

GPU-AVはshaderへの計装とGPU資源の検査を行い、速度・descriptor枠・feature有効化へ影響する。
固定SDKと最新VVLでは要件や設定の説明が変わるため、実際に使う版の起動状態と有効な検査範囲を残す。
GPU-AV用の設定でCoreを減らす場合も、Core＋SyncValによる通常受入は別に維持する。
計装された実行を通常の性能値として報告しない。
[LunarG: SDK 1.4.341.1 GPU-AV](https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/gpu_validation.html)

Best Practicesは性能上の疑い・推奨を通知する機能で、全warningが仕様違反を意味するわけではない。
対象vendor・実装・負荷を調べ、機械的に全推奨へ書き換えない。
本リポジトリの実GPU受入では追加検査も含めwarningを黙って無視せず、確認済み誤検出の個別扱いは既存方針に従う。
[LunarG: Best Practices Validation](https://vulkan.lunarg.com/doc/view/latest/windows/best_practices.html)

Vulkan ConfiguratorのSynchronization presetは他の検査を無効化する場合がある。
`validate_sync=true`を追加したことと、presetを選んだことは同じ保証ではない。
レイヤーの起動情報から実効設定を確認し、環境設定の上書きも記録する。
具体的な受入・例外・実行コマンドは[開発手順](development.md)に従う。
[LunarG: SDK SyncVal設定](https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/synchronization_usage.html)

### GPUで起きた処理をGraphへ対応づける

`VK_EXT_debug_utils`でresource、pipeline、descriptor、queue、Fenceなどを命名し、
command bufferへGraph/pass単位のregionを付ける。Graphダンプの論理IDとnative object名を対応させる。
検証を無効にした性能実行でも必要な名前・regionを保てる設計にする。
[Khronos: debug_utils](https://docs.vulkan.org/guide/latest/extensions/VK_EXT_debug_utils.html)

regionは釣り合うbegin/endを持ち、実行間で比較できる安定したpass名を使う。
頻繁に変わる番号だけを名前にすると、ツールで同じ処理を比較しにくい。
通常はpassや大きな処理を注釈し、ツールが認識できるすべてのdraw/copyへ冗長なregionを増やさない。
[NVIDIA: Performance Events](https://developer.nvidia.com/blog/best-practices-gpu-performance-events/)

debug callbackからVulkan commandを呼んではならず、アプリケーションは`VK_FALSE`を返す。
RustのpanicをFFI境界から外へ出さず、subscriberからのVulkan再入や保持ロックとのdeadlockも防ぐ。
callback内データを後で使う場合は所有データへコピーし、callbackとmessengerの寿命を保持する。
後半のRust/ログ処理は本リポジトリの所有方針である。
[Khronos: Callback VUID](https://docs.vulkan.org/refpages/latest/refpages/source/PFN_vkDebugUtilsMessengerCallbackEXT.html)

### CPU時間とGPU時間を分ける

計測はまずCPU制約かGPU制約かを確認する。CPUのrecord/submit時間、GPUの実行区間、
submitから結果利用までのlatency、一定期間のthroughputは別の指標として扱う。
CPUがGPUへ仕事を渡せていない状態で、shaderの最適化だけから全体速度を結論づけない。
[NVIDIA: Expected Workflow](https://docs.nvidia.com/nsight-graphics/UserGuide/expected-workflow.html)

GPU timestampは使用queue familyの`timestampValidBits != 0`を確認し、
`timestampPeriod`でtick差を時間へ変換する。記録するstageはqueueが対応するものを選ぶ。
64-bit結果を取得しても有効bit数は増えず、差分は有効bit幅のwrapを考慮する。
複数回wrapし得る長い区間を、単一wrap前提の差分で測らない。
[Khronos: Timestamp Queries](https://docs.vulkan.org/spec/latest/chapters/queries.html#queries-timestamps)

timestampは要求したstage、または実装がそれより論理的に後のstageで書かれ得る。
パイプラインの重なりを考慮した意味のある区間を選び、隣接したvertex/fragment timestamp差を
そのshaderだけの占有時間とみなさない。比較はまず同一queue上の同条件の区間で行う。
[Khronos: Timestamp Sample](https://docs.vulkan.org/samples/latest/samples/api/timestamp_queries/README.html)

queryは初回・再利用前にresetし、使用中のqueryをresetしない。
結果は完了またはavailabilityを確認して取得する。`WAIT_BIT`による取得時の待機を、
通常frameの隠れたCPU/GPU直列化にしない。host resetには`hostQueryReset`の有効化と対象処理の完了が必要である。
query slotの再利用もGPU完了条件で管理する。
[Khronos: Queries](https://docs.vulkan.org/spec/latest/chapters/queries.html)

CPU時刻とGPU時刻を結びつける場合は、対応するtime domainと任意拡張
`VK_KHR_calibrated_timestamps`等を確認し、取得した`maxDeviation`も扱う。
CPUの時計値とGPU tick値を直接引き算しない。異なるqueueの数値比較にも共通時刻域・順序の前提を確認する。
[Khronos: Calibrated Timestamps](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetCalibratedTimestampsKHR.html)

pipeline statistics/occlusion queryは必要featureとqueue上の使用条件を確認する。
統計値は同一実装内の相対的な調査に使い、異なるGPUのカウンター値を同じ仕事量と仮定しない。
詳細queryやcaptureが負荷を変える可能性もあるため、最終的な改善は通常実行でも再測定する。
[Khronos: Pipeline Statistics](https://docs.vulkan.org/spec/latest/chapters/queries.html#queries-pipestats)

### 計測の再現性と最適化判断

本リポジトリでは次を一つの比較記録として揃える。

- commit、build profile、CPU/GPU/driver/OS、SDK・compiler、shaderとpipeline設定。
- 解像度、入力データ、pass列、seed、転送量、in-flight数、capacity、present/VSync条件。
- Validation・GPU-AV・capture・profiler・ログの状態。比較相手と同条件にする。
- warm-up、測定回数、CPU各phaseとGPU区間、中央値・個別sampleのp95、run間変動。
- native作成・再利用・submit回数、stagingのhigh-water、VMA block/Allocation・budgetの区別。
- 正しさの照合方法と計測区間。shader初回コンパイルやcache cold/warmも区別する。

この記録様式と通常CIに固定の壁時計閾値を置かない方針は、本リポジトリの判断である。
GPUの電力・温度・clock・他プロセスの干渉も測定へ影響する。clock固定等を行った場合はその設定を残す。
設定変更を前提にせず、通常の利用環境との差も報告する。
[NVIDIA: GPU Trace Overview](https://docs.nvidia.com/nsight-graphics/UserGuide/gpu-trace-overview.html)

submitの集約はCPU overheadを減らし得る一方、記録完了までGPUへ渡さなければlatencyやidleが増える。
小さすぎるcommand bufferと過度な一括化の双方を、CPU/GPU timelineから判断する。
最少submit数を固定目標にせず、latency・throughput・CPUコストを比較する。
[NVIDIA: Vulkan Dos and Don'ts（NVIDIA向け、2025年更新）](https://developer.nvidia.com/blog/vulkan-dos-donts/)

vendorごとの勧告はGPU世代と負荷の条件を持つ。たとえばMali CSFの資料は特定の世代を対象とし、
fragment/non-fragmentの重なりやbarrier統合による余計な依存を説明する。
そのhardware stageへのまとめ方を、RTX向けGraph意味論や全vendor共通の最適解にしない。
共通の正しさを維持し、native計画の変更を対象機材で測る。
[Arm: Mali synchronization（対象世代の明記あり）](https://developer.arm.com/community/arm-community-blogs/b/mobile-graphics-and-gaming-blog/posts/vulkan-synchronization-how-pipeline-barriers-map-to-mali-gpu-hardware)

### 調査ツールの選択

| 調査対象 | 公式ツール・資料 | 判断するもの |
|---|---|---|
| API state、binding、buffer/imageの内容 | [RenderDoc](https://github.com/baldurk/renderdoc) | frame captureによる描画不具合の調査 |
| NVIDIA上のGPU units、queue重なり、shaderの制約 | [Nsight Graphics GPU Trace](https://docs.nvidia.com/nsight-graphics/UserGuide/gpu-trace-overview.html) | GPUのidle、同期、throughput制約 |
| AMD上のevent timing、pipeline stall、async compute | [Radeon GPU Profiler](https://gpuopen.com/manuals/rgp_manual/) | RDNA上のnative実行の調査 |
| Intel上のCPU/GPU/API、frame/trace | [Intel GPA](https://www.intel.com/content/www/us/en/docs/gpa/get-started-guide/2025-1/overview.html) | 対応API/OS/driver範囲を確認した調査 |
| Arm上のhardware counters、熱・帯域、tile構成 | [Arm Performance Studio](https://developer.arm.com/Tools%20and%20Software/Arm%20Performance%20Studio) | StreamlineとFrame Advisorによる機材固有の調査 |

toolの対応GPU/driver/OSは採用時に確認する。ここへの掲載は導入・使用済みを意味しない。
現環境で導入確認したRenderDocの実行方法と、まだ実GPU captureを行っていない事実は[ツールガイド](tooling.md)にある。
capture/replay・instrumentation・counter収集の実行は、通常実行の最終性能値と区別して記録する。

<a id="optimization"></a>

## 最適化の判断と後続機能

### 描画負荷を減らす判断

**条件付き最適化:** drawの集約・pipeline順の並べ替え、頂点streamの構成、
不要なusage flagの削減、render targetの精度・帯域・圧縮維持を比較候補にする。
不透明描画の状態変更削減と深度による遮蔽の利益、透明描画の順序要件は別に扱う。
depth prepass、discard、full-screen処理をComputeへ移す選択も、追加のgeometry負荷や帯域を含めて測る。
これらはRDNA向けの推奨から得る候補であり、特定のblock容量・workgroup数・VRAM使用率を全GPU共通の定数にしない。
同資料のDirectX列のheap・root signature・residency APIもVulkanへ直訳しない。
[AMD RDNA Performance Guide](https://gpuopen.com/learn/rdna-performance-guide/)。

**条件付き最適化:** texture圧縮は対応形式、用途、画質を確認して選び、
転送・メモリ容量とsampling時のGPU時間を比較する。
圧縮blockのコピー条件と、storage/attachment用途に必要なformat能力は別に確認する。
色・法線・数値データごとの品質も比較対象へ含める。
[Khronos: Texture compression comparison](https://docs.vulkan.org/samples/latest/samples/performance/texture_compression_comparison/README.html)。

**本Renderer:** 最適化は、入力と出力の意味を維持した基準経路との比較として導入する。
CPUの仕事をGPUへ移す場合も、新しいGPU負荷、readback待機、キャッシュ規模、メモリhigh-waterを測る。
「draw数が減った」「barrier数が減った」「FPSが一度上がった」だけでは採用根拠にしない。
culled pass、overdraw、bandwidth、occupancy等は調査の手掛かりであり、単独の最大化・最小化目標ではない。

### 高度な経路を導入する際の追加契約

以下は後続機能の調査入口であり、M2の実装済み機能や完了条件へ追加する一覧ではない。
具体的な導入時は、専用の設計・受入条件と対象GPUでの計測を用意する。

| 経路 | 導入時に追加で確認すること | 一次資料 |
|---|---|---|
| GPU生成indirect draw | indirect buffer usage、offset/stride/サイズ、`multiDrawIndirect`等のfeatureとcount上限。Computeで生成した引数から`DRAW_INDIRECT`／`INDIRECT_COMMAND_READ`への依存。可視resource集合と生成データの寿命 | [Multi-Draw Indirect](https://docs.vulkan.org/samples/latest/samples/performance/multi_draw_indirect/README.html)、[vkCmdDrawIndexedIndirect](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdDrawIndexedIndirect.html) |
| Mesh Shader | extension・task/mesh feature、workgroup・出力・payload上限、SPIR-V、pipeline・stage/accessとGraph宣言。従来のvertex/index経路との品質・負荷比較 | [VK_EXT_mesh_shader](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_mesh_shader.html) |
| Ray tracing／acceleration structure | build入力・scratchのalignmentと範囲、build/updateの条件、build間・trace前の依存、TLASから参照するBLASの寿命。複数buildを一命令へ渡しても相互同期は自動成立しない | [vkCmdBuildAccelerationStructuresKHR](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdBuildAccelerationStructuresKHR.html) |
| Sparse residency | sparse featureとqueue能力、page/mip tail条件、bind変更と利用queueの同期、mappingと物理memory寿命。sparse bindは通常bindと異なるqueue操作 | [Sparse Resources](https://docs.vulkan.org/guide/latest/sparse_resources.html) |
| 外部memory／semaphore共有 | handle typeごとのimport/export対応、OS handleの所有とclose責任、dedicated要件、外部からの内容・layout・所有権・完了の引継ぎ。memory共有だけで同期は共有されない | [External Memory and Synchronization](https://docs.vulkan.org/guide/latest/extensions/external.html) |

複数キュー・物理aliasing・bindless・descriptor buffer・defrag・BDAは、
それぞれ本書の該当節と同じく「featureがあること」と「正しく使えるruntimeがあること」を分ける。
Video、保護memory、device group、プラットフォーム固有の表示連携等の全拡張の個別実装手順は本書の対象外である。

<a id="application"></a>

## 本Rendererへの適用とレビュー基準

### 現在の契約と機能導入の境界

| 領域 | 現在の基準 | 機能を導入・変更するときの確認 |
|---|---|---|
| Instance/Device | Vulkan 1.3、必要3 feature、Graphics＋Compute単一queue | 新しいfeature・format・limitを照会し、使用と有効化を対応させる |
| allocation/CPU access | VMA既定pool、所有型、flush/invalidate、atom分離 | 新しいusageとABI・copy制約、実際のmemory typeを確認する |
| execution/staging | 上限付きpool、batch、shared lease、実Fence完了後回収 | 記録中・pending・完了・CPU消費を分け、資源種類を増やしても同じ回収根拠を保つ |
| Graph | 未実装。M2で導入 | producerの意味と同期辺、生存解析、内容有効性、canonical import、予定／完了状態を分離する |
| Shader/Compute/描画 | 未実装。M2で導入 | bindingと宣言の照合、ImageView/pipeline/descriptorの保持再利用、実際の用途に合う同期を実証する |
| Presentation/複数frame | 未実装。M3で導入 | frame資源とimage別present資源、acquire後キャンセル、旧Swapchain世代の完了を区別する |
| 第二API/高度な最適化 | 後続段階 | 同じ論理Graphの出力・性能を実機比較し、共通契約とAPI固有条件を抽出する |

この表は現行[Vulkan設計](../design/vulkan.md)と[実装方針](../design/implementation-policy.md)への
調査結果の適用であり、コード全体の再監査や追加GPU実行の合格記録ではない。
課題の状態・依存関係・次の実装対象はBeadsを参照する。

### 変更のレビューで答える問い

1. 使用するAPI・extension・feature・format・limitは、照会と有効化の両方で成立しているか。
2. どの内容を読むか、誰が最後に書いたか、どのbyte/subresourceが有効かを説明できるか。
3. 実行順、availability/visibility、layout、queue所有権、CPUアクセスの各条件に根拠があるか。
4. 記録済み未submit、pending、timeout、cancel、OOM、Device lossの各状態で、所有者と回収条件が明確か。
5. descriptor更新、pool reset、pipeline/View破棄、staging上書きは、参照する命令とCPU消費の寿命を満たすか。
6. import/exportは同じ物理資源の識別と予定状態を引き継ぎ、CPUに返したhandleを即時readyと誤認させないか。
7. 正しさの内容照合、Core＋SyncVal、CPU境界試験、必要な追加診断を別々に実行・報告しているか。
8. 性能主張に同条件の測定があり、CPU/GPU/latency/メモリの改善と代償、対象vendorと世代を説明できるか。

確認済み誤検出以外のValidation warning/errorを許容しない方針、
実GPU試験を`#[ignore]`で分離する方針、GPUが使えない場合の報告は[開発手順](development.md)に従う。

<a id="sources"></a>

## 資料の選び方と更新

本文の各リンクは、その近くの主張を支える一次資料である。
資料間で助言が異なる場合は、まず仕様の正しさを満たし、GPU世代・負荷・API版の違いを調べる。
一方の性能上の助言を他方の仕様要件より優先しない。

| 資料群 | 用途 |
|---|---|
| Khronos Specification / API Reference | Valid Usage、同期・寿命、feature・extension・返値の正式な契約 |
| Khronos Vulkan Guide | APIの使い方、移植上の落とし穴、概念の整理 |
| Khronos Vulkan Samples | 小さな実行例と条件付きの性能比較。sampleの数値はこのRendererの実測ではない |
| 固定SDKのLunarG/VVL資料 | 実際の検証設定、検査対象と限界。latest設定との混在を避ける |
| VMA公式と同梱版ソース | allocatorが提供する機能と、Rendererに残る同期・寿命の責任 |
| shaderc / SPIRV-Tools公式 | shaderの生成・検証条件。実行時の同期や安全性の代替にはしない |
| NVIDIA / AMD / Intel / Arm公式 | 対応機材の最適化候補と解析ツール。vendor横断の保証として扱わない |

調査日以降にSDK・driver・ash・vk-mem・compilerや対象GPUを変えた場合は、
影響する要件と参照先を再確認する。採用判断は現在の設計文書、実行した結果は
[現在の検証状況](../verification/current.md)へ反映し、本書を時系列の作業記録にしない。

# Vulkanバックエンドの設計

現在のVulkan基盤とGraph実行で有効な契約と、その判断理由をまとめる。
全体の構成と実装順序は[実装方針](implementation-policy.md)、
実行コマンドは[開発手順](../guides/development.md)、
確認済みの環境・結果・制限は[最新の検証状況](../verification/current.md)を参照する。

一般的な実装・レビューの根拠と、M2以降の機能導入時に確認する条件は
[Vulkanベストプラクティス](../guides/vulkan-best-practices.md)へまとめる。
同ガイドの後続機能は、この文書の現行実装に追加済みとは扱わない。

## 実装境界

`crates/renderer`の`vulkan` feature配下に、初期化、診断、VMAによるbuffer／image所有、
単一キューのcopy・Compute・Dynamic Rendering、投入・完了管理、資源再利用を実装する。
ashはVulkanローダーを実行時に読み込み、通常のCPUライブラリビルドへSDK依存を広げない。
CPUのテスト・benchmark用依存が要求するツールは開発手順で分けて管理する。

ネイティブ型や生のハンドルはバックエンド内部に閉じる。
未使用の公開RHI、汎用encoder、Backend traitを先に固定しない。
公開Graph APIはGPU非依存で、Vulkanの実行は`VulkanRuntime`から明示的に開始する。
固定shaderによるCompute、indexed meshと深度、sampled imageの最終合成を扱う。
Graphの宣言・producer・範囲・culling・import/exportは[RenderGraphの契約](rendergraph.md)を参照する。
Swapchain、表示、任意shader、一般的なPassEncoderは未実装である。
Deviceで機能を有効にしていることと、その機能を使う処理が実装済みであることを区別する。

## Instance・Device・キュー

InstanceとVMAが要求するAPIはVulkan 1.3とする。
初期化前にAPI、Layer、extension、物理デバイスの能力を照会する。
取得できた不足条件をまとめ、照会失敗で不明な条件を不足と決めつけない。
エラーには操作と元の原因を残す。

物理デバイスにはAPI variant 0のVulkan 1.3以上、Graphics＋Compute対応キュー、
`timelineSemaphore`、`synchronization2`、`dynamicRendering`を要求する。
対応確認とDevice作成用のfeature構造体を分け、必要な3 featureだけを有効化する。

適合候補は専用GPU、内蔵GPU、仮想GPU、その他、CPU型の順で自動選択し、同順位は列挙順とする。
CPU型も最後の候補として利用できるが、その選択をログに残す。
実GPU受入試験はCPU型の成功で代替しない。
選択後のDevice作成失敗は、そのデバイスと原因を報告して停止し、別候補へ自動再試行しない。
これにより、能力不足による除外と実行時の作成失敗を混同しない。

最初の適合queue familyからindex 0のキューを1本取得する。
family番号を固定せず、共有Queueのホスト操作はDevice所有のロックで直列化する。
複数キューの割当・同期・所有権移譲・寿命管理を必要になる前に増やさず、
現在の単一キューを基準として後続の実処理を比較する。

作成したnative資源は直ちに所有型へ収め、後続の命名や情報照会が失敗しても解放する。
DeviceはInstanceを所有し、InstanceはEntryと診断状態を維持する。
messengerはInstanceより先に破棄し、Instanceの作成・破棄中もcallbackを受け取れる寿命を保つ。
debug_utilsのDevice用関数テーブルは一度保持し、Queueも作成時に一度だけ命名する。

## 検証と診断

既定の`ValidationMode::Required`はValidation Layer、debug_utils、layer_settingsを要求し、
core validationを維持したままSynchronization Validationを追加するよう指定する。
受入試験ではレイヤー起動情報を確認し、外部設定で検証を無効にした成功を受け入れない。

性能測定には`ValidationMode::Disabled`を明示できる。
この場合はValidation Layerとlayer_settingsを要求せず、debug_utilsと診断を維持する。
releaseだけで暗黙に検証を外さない。外部loader設定によるLayer注入は別途確認する。

`tracing`のsubscriber・出力先・表示レベルは利用側が設定する。
ライブラリはglobal subscriberを設置せず、初期化時の`Dispatch`を保持して別スレッドのcallbackにも使う。
GENERAL／VALIDATION／PERFORMANCEの全重要度を受信し、ログフィルターとは独立して件数を数える。
直近64件と最初のwarning／error 128件を保持し、保持できなかった問題メッセージの件数も示す。

callbackは文字列を所有データへコピーし、集計ロックを解放してからsubscriberを呼ぶ。
callbackとsubscriberからVulkanへ再入しない。panicはFFI内に封じ、常に`VK_FALSE`を返す。
subscriberがpanicした後は外部ログ転送を停止し、集計を続ける。
破棄時にもpanicし得るpanic payloadは、この異常経路では保持する。

実Vulkan受入試験は終了後までwarning、error、callback内部失敗のゼロを要求する。
例外は個別に根拠を記録した確認済み誤検出だけで、現在の例外はない。
転送ごとの詳細はdebug、初期化情報と異常は通常の診断へ記録し、
object名・command label・Fenceを調査時の対応づけに使う。

## メモリ所有とCPUアクセス

VMAの既定pool・メモリタイプ選択・suballocationを利用する。
物理メモリの割り当てアルゴリズムや、用途のないcustom poolを独自実装しない。
`MemoryOwner`はVMAをDeviceより先に破棄し、Deviceはその後でInstanceを解放する。

buffer／imageとAllocationの組は単独所有し、`Arc<MemoryOwner>`で依存の寿命を保持する。
Allocatorの操作用ハンドルを先に破棄しても資源を無効にしない。
`mem::forget`でも所有者への参照が残り、未解放Allocationの下でVMAだけを破棄しない。
この参照共有自体はGPU完了を保証しない。

通常のbuffer用途は次の3種類である。

| 用途 | Vulkan usage | VMAの選択 | CPUアクセス |
|---|---|---|---|
| Upload | transfer source | AutoPreferHost、sequential write | write |
| DeviceTransfer | transfer source／destination | AutoPreferDevice | 不可 |
| Readback | transfer destination | AutoPreferHost、random access | read／write |

CPU APIは渡されたバイト列をコピーする`write`／`read`とし、mapped pointerを返さない。
bufferの要求sizeを基準に範囲・加算・変換を検証し、Allocationの余白を利用可能領域とみなさない。
転送用・共有staging・GraphのbufferはVulkan 1.3の`maxBufferSize`も照会し、
上限を超えるsizeをVMAへ渡す前に拒否する。CPUアドレス空間の上限やnative OOMとは区別する。
全面ゼロ埋めの代わりに確立済み範囲を追跡し、穴を含むreadを拒否する。
write失敗時に旧内容へ戻る保証は設けない。

成功したmapはguardで管理し、失敗やunwind時もunmapする。
非coherentメモリではwrite後のflushとread前のinvalidateを行い、coherentなら省略する。
VMAへはAllocation先頭からの相対offsetを渡し、cache範囲の丸めを任せる。
invalidate失敗時にCPU出力を書き換えない。

共有Upload bufferはVMAの常時mapを使い、コピーと必要なflushを維持する。
Readbackはscoped map・invalidate・copyとする。
VMAのmap／unmap呼び出しを、毎回必ずnative mappingを作り直す処理とはみなさない。

## 画像とcopyの範囲

既存の転送試験用`VulkanImage`はRGBA8_UNORM、2D、optimal tiling、mip・layer・sample各1、
transfer source／destination、EXCLUSIVE共有、初期layout UNDEFINEDに限定する。
作成時に実際のformat・type・tiling・usage・flagsで能力を照会し、返された上限を確認する。
optimal imageの必要メモリ量をpacked pixel byte数から推定せず、native作成時のOOMも扱う。

画像転送は全体をR・G・B・A各1 byte、左から右、行順の隙間のない列として扱う。
byte数は`幅 × 高さ × 4`をchecked計算する。色変換やファイル形式への変換は行わない。
バッチは共有buffer内のoffsetを使えるが、画像の部分領域、行間の余白、複数mip・layerは扱わない。

buffer copyは範囲と初期化状態を確認し、同一bufferの重複するsource／destinationを拒否する。
copyのoffsetは4-byte境界、sizeは非ゼロを要求する。
upload-only、readback-only、複数copyの一括投入、同一バッチ内の再書き込みに対応する。
既存の転送payloadは画像のlayoutと初期化状態を記録時に先取りせず、実Fence完了後に確定する。
そのCOPY専用計画と、下記のGraphの予定／完了状態は別の実行契約である。

GraphのRGBA8_UNORM画像はtransfer source／destination、storage、sampled、color attachmentの
固定usage組を持つ。D32_SFLOATはdepth attachmentとして使う。
両者とも2D・optimal tiling・mip／layer／sample各1とし、実際のusage組の能力とextent、
framebuffer／viewport上限を照会する。Graph画像はallocationとImageViewを一緒に保持する。
固定usageはexport後に対応操作へ再importできるようにするためであり、
usage削減による最適化や、未確認GPUでの対応を主張しない。

初回のnative画像layout遷移には、先行COPYを待つexecution依存を置く。
未使用画像のUNDEFINEDからの遷移に、Vulkan仕様がこの依存を一律要求するわけではない。
RTX 3060／driver 610.88では、buffer upload→小画像upload→buffer readbackでbuffer内容が
ゼロになる経路を観測し、初回画像遷移のsourceをCOPY、accessをNONEとすると解消した。
修正前の実装でも再現し、Validation無効、transfer-only画像、別の専用Allocationでも再現した。
VMAの配置は実測のbufferImageGranularityを満たしており、原因をaliasing違反と断定しない。

これはdriver内部の原因が未確定の、出力の正しさを保つための保守的な対策である。
全体idleやhost待機を追加せず、既存のwriter可視性も維持する。
同じキューの先行submitも対象にできるため、Graph内だけの初期遷移の前倒しより優先した。
物理画像を初めて使う際に独立したCOPYとの重なりを減らす可能性があり、GPU時間への影響と
別GPU／driverでの比較は未計測である。再利用時の既存layout遷移にはこの追加依存を適用しない。
回帰試験と追跡先は`renderer-j17`に記録する。

## GraphのShaderと描画

管理下のGLSLと生成済みSPIR-Vを保持し、通常のRustビルドへshaderコンパイラーを要求しない。
固定SDKでVulkan 1.3をtargetにし、SPIR-V 1.3を明示する。
Deviceで有効にしていない`maintenance4`の`LocalSizeId`へ暗黙に依存しない。
再生成とbinary一致確認は[開発手順](../guides/development.md)に従う。

Computeは8×8のlocal size、set 0のbinding 0／1に各1個のRGBA8 storage imageを使う。
push constantはoffset 0のu32 1個で変換を選ぶ。端数のinvocationはshader内で除外し、
hostでもworkgroup size・invocation数・dispatch数・descriptor・push constantの上限を確認する。
合成はset 0 binding 0のcombined image samplerから同じ座標を`texelFetch`し、
別の同寸画像へfullscreen triangleで書く。filterやsRGB変換はしない。

pipeline・layout・samplerはCompute／Raster／Compositeの必要な種類ごとに遅延作成し、
成功した不変のProgramをruntimeで保持する。不使用の種類の能力を要求せず、native資源も作らない。
RasterのD32 depth対応を含む形式・ABI上限を、その種類のnative作成前に検査する。
公開したProgramを置き換えないため、未完了Computeがある間にもRaster等を追加できる。
作成に失敗したProgramをキャッシュせず、部分作成分を解放して再試行可能な状態を維持する。
descriptor arenaは同時実行数とGraph内binding数に上限を持ち、
GPU完了または未投入キャンセルを確認してから再利用する。
更新済みdescriptorを参照する記録済み／未完了命令を、別の更新で無効にしない。
pipeline作成に失敗した際も、部分的に作成できたhandleを解放する。

頂点はclip座標xyz（W=1）とlinear rgba、深度範囲は0〜1の通常Z、
clear depthは1、compareはLESS、viewportの高さは正、cullingは無効とする。
front faceはCOUNTER_CLOCKWISEを設定するが、cullingで判定を使う段階では向きも再検証する。
行列・座標系変換・sRGB／HDR・tone mappingはまだ提供しない。
色はlinear RGBA8_UNORMで、alphaを保持し、blendingは無効とする。
u32 indexは頂点bufferの範囲と、起動時に保持した実際の`maxDrawIndexedIndexValue`の両方を検査する。
Vulkan 1.3の全adapterで32bit全域を使えるとは仮定せず、feature有効化だけで上限を推定しない。

## 投入・完了・異常時の保持

`submit`と`wait`を分け、記録済み・未完了の所有者が命令から参照される全資源を保持する。
所有する対象をGPU実行中にCPUへ返さず、通常のアクセス・解放・再利用との競合を避ける。
投入前にpending状態を確立し、submit後のログ出力がunwindしても安全性を失わない。

正常完了として内容を返す条件は、対象Fenceの実際の成功待機とDevice activeの両方である。
timeoutは未完了として資源を保持し、再試行できる。一般的な待機エラーも完了証明にしない。
正常経路では対象Fenceだけを待ち、Queue／Device全体のidle待ちを使わない。

submitのメモリ不足は仕様の状態不変保証に従う未投入の失敗として扱う。
未投入や完了を証明できない投入失敗は、pendingの所有と同じ回収規則で扱う。
`VK_ERROR_DEVICE_LOST`はDeviceの終端状態とし、自動復旧や有効データとしての返却をしない。
他の処理が先にlossを検出しても、当該Fenceの実際のwait結果を得てから後始末する。
停止・loss後は新規投入、通常の資源作成・CPUアクセスを拒否し、既存処理の完了確認と後始末を続ける。

未完了のDropでは`vkWaitForFences`へ10秒のAPI待機予算を渡す。
これは壁時計の厳密な上限ではない。完了を証明できなければDeviceを停止し、
native資源・payload・VMA・Deviceを含む所有連鎖を保持して早期解放を避ける。
この異常経路では保持した資源を回収できない。
通常の回収は明示的な完了確認で進め、Drop待機へ依存しない。

## 実行資源と共有stagingの再利用

`ExecutionPool`はCommandPool・一次CommandBuffer・Fenceの組を上限付きで遅延作成する。
予約を先に取得でき、空き不足は待たずに返す。待機・再試行は呼び出し元が選ぶ。
未投入キャンセルまたは実Fence完了後にpoolとFenceをresetし、成功した枠だけを再利用する。
reset失敗やDevice停止・lossでは再貸出しない。
貸出中のleaseがpool状態を保持し、idle資源からpoolへの循環参照は作らない。

`TransferPool`はUpload／Readback各1本の固定容量bufferを共有する。
内部の領域配置・分割・結合はVMA Virtual allocatorへ任せ、Rendererはleaseの寿命を管理する。
バッチ専用bufferより余剰容量を共有しやすくする一方、GPU完了とCPU利用終了を区別して回収する。

容量はCPU-addressableな非ゼロ値で、`max(4, nonCoherentAtomSize)`の倍数を要求する。
領域の位置と予約長も同じ境界へ合わせ、隣のleaseとcache atomを共有しない。
要求byte数と整列した予約byte数を区別し、空き不足で内部待機や無制限な増設を行わない。
この容量はbufferの要求byte容量であり、VMAの物理blockやDevice全体のVRAMの上限ではない。
virtual領域の空き不足とnativeメモリ確保のOOMも区別する。

Upload leaseは正常完了時に返す。Readback leaseはCPUコピー成功または結果破棄まで保持し、
読み取り失敗では再試行できる。共有Readbackの領域APIはCPU read用とする。
native実行枠は未読Readbackより先に再利用できる。
再取得した領域の過去のbyte列を、新しい要求の初期化済み内容として扱わない。
完了不明の処理はleaseを含めて保持し、領域だけを先に再利用しない。

## 範囲計画と性能評価

`ByteRanges`は半開区間を`BTreeMap`で保持し、隣接・重複を統合する。
初期化追跡とcopyの競合判定で共用し、保持区間数Kに対する包含・重複検索をO(log K)、
N件の挿入列を償却O(N log N)で扱う。独自の木構造や空きメモリ配置は実装しない。

競合判定はbufferごとにreadとwriteの集合を分ける。
同一copyの全アクセスを先行copyと照合してから挿入し、RAW／WAR／WAWに依存を入れる。
read同士と独立範囲を各copyごとに直列化しない。
global barrierが必要なら全bufferの先行アクセスを追跡集合から消す。
HOST依存はReadbackを含むpayloadにだけ記録する。

CPUの範囲計画はCriterionで測定し、実際のprivateな範囲集合と競合判定のソースを使う。
比較用の基準実装はbenchmark内に限定し、内部APIを公開しない。
GPUの区間別時間、timestamp、個別転送のp95と内容照合は、明示実行のGPU計測で扱う。
CPU時間、GPU時間、完了までのlatency、throughputを区別し、描画FPSへ読み替えない。
性能値とその揺れは最新の検証状況へ記録し、通常のCIに壁時計時間の固定合否閾値を設けない。

## 参照する仕様

- [キューとホスト同期](https://docs.vulkan.org/guide/latest/queues.html)
- [投入・状態・失敗時の保証](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html)
- [Lost Deviceの扱い](https://docs.vulkan.org/spec/latest/chapters/devsandqueues.html#devsandqueues-lost-device)
- [VMAのメモリmapping](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/memory_mapping.html)
- [VMA Virtual allocator](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/virtual_allocator.html)
- [Command資源の再利用](https://docs.vulkan.org/samples/latest/samples/performance/command_buffer_usage/README.html)

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
Windows表示はoptionalな`presentation` featureで追加する。
任意shader、一般的なPassEncoderは未実装である。
Deviceで機能を有効にしていることと、その機能を使う処理が実装済みであることを区別する。

## Instance・Device・キュー

InstanceとVMAが要求するAPIはVulkan 1.3とする。
初期化前にAPI、Layer、extension、物理デバイスの能力を照会する。
取得できた不足条件をまとめ、照会失敗で不明な条件を不足と決めつけない。
エラーには操作と元の原因を残す。
要求extensionはC文字列の内容で重複除去・整列してからFFI用pointerへ変換する。
同名でも保存場所が異なり得るため、pointerの数値を名前の同一性や順序に使わない。

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

Windows表示の初期化はInstance→Win32 Surface→実Surface対応adapter/queue→Deviceの順とする。
同じGraphics＋Compute queueでpresentできる候補だけを採用し、別present familyは対応外として診断する。
Presentationのみswapchain、EXT swapchain maintenance1とその依存拡張・featureを要求する。
固定ashで利用できるEXT版を選び、headlessの必要拡張・featureは増やさない。

作成したnative資源は直ちに所有型へ収め、後続の命名や情報照会が失敗しても解放する。
DeviceはInstanceを所有し、InstanceはEntryと診断状態を維持する。
messengerはInstanceより先に破棄し、Instanceの作成・破棄中もcallbackを受け取れる寿命を保つ。
debug_utilsのDevice用関数テーブルは一度保持し、Queueも作成時に一度だけ命名する。

## 検証と診断

製品・開発・性能・障害調査の分類は[検証方針](../guides/validation-policy.md)に従う。
製品はValidation Layersを既定無効とし、開発用検証へ依存しない実行を目指す。
次の既定値・切替は現行実装の説明であり、この文書変更で内部検証を除去したものではない。

既定の`ValidationMode::Required`はValidation Layer、debug_utils、layer_settingsを要求し、
core validationを維持したままSynchronization Validationを追加するよう指定する。
受入試験ではレイヤー起動情報を確認し、外部設定で検証を無効にした成功を受け入れない。

製品でのLayer無効化と性能測定には`ValidationMode::Disabled`を明示できる。
この場合はValidation Layerとlayer_settingsを要求せず、debug_utilsと診断を維持する。
releaseだけで暗黙に検証を外さない。外部loader設定によるLayer注入は別途確認する。

この切替はGraph検査、index全走査、CPU provenance、debug名等を除去する機能ではない。
内部の開発用検証を製品から除外するビルド切替と、診断拡張を不要にする経路は未実装である。
今後は最適化と追加検証を独立に選び、同期・寿命・容量・native失敗の処理を維持したまま保証を上流へ移す。
[現行との差分](../guides/validation-policy.md#current)を参照する。

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

公開`ValidationMonitor`は診断状態だけを保持し、Instance・Deviceの寿命は延ばさない。
native所有者を破棄した後にも、警告・エラー・callback失敗と問題原文を確認できる。
Layer起動状態は直近messageとは別にdistinct原文を最大8件保持し、overflowも報告する。
固定SDKの認識できる一貫した起動報告だけからCore／SyncValの実効設定を判断する。
欠落、未知形式、矛盾、切断、overflowは未確認として扱う。
Disabledを要求した際の起動報告の不在から、外部Layer注入が絶対にないとは断定しない。
`is_clean()`の件数ゼロと、実際に検証が有効だった証明も別の条件である。
`device_info()`はadapter・API・driver情報の所有値を返し、vendor固有のdriver versionを
Vulkan API versionの形式として誤decodeしない。

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

頂点はmodel座標xyz（入力W=1）とlinear rgbaで、描画ごとのMVPでclip座標へ変換する。
既存`Graph::raster`はidentity MVPと白tintを使うため、従来のclip-space入力を維持する。
頂点shader用push constantはcolumn-major mat4がoffset 0、linear tint vec4がoffset 64の計80 byte。
実Deviceの上限を照会し、pipeline layoutと記録のstage・offset・sizeを一致させる。
色と行列はGraphの不変operandとして保持し、後続frameのCPU入力変更に依存しない。
深度範囲は0〜1の通常Z、
clear depthは1、compareはLESS、viewportの高さは正、cullingは無効とする。
front faceはCOUNTER_CLOCKWISEを設定するが、cullingで判定を使う段階では向きも再検証する。
汎用projection builder、HDR、tone mappingは提供しない。
色はlinear RGBA8_UNORMで、alphaを保持し、blendingは無効とする。
u32 indexは頂点bufferの範囲と、起動時に保持した実際の`maxDrawIndexedIndexValue`の両方を検査する。
Vulkan 1.3の全adapterで32bit全域を使えるとは仮定せず、feature有効化だけで上限を推定しない。

標準opaqueの`RasterBatch`は一つのDynamic Rendering区間でcolor/depthをClear/Storeし、
pipelineとviewportを設定してから、owned draw列の順にbuffer binding・80-byte引数・indexed drawを記録する。
draw命令自体はdraw数分あり、instancingやmulti-drawへの変換、描画順の並べ替えは行わない。
空listにはclear-onlyの既存経路を使い、Raster Programの作成を要求しない。
参照用の単一draw Rasterも同じoperand検査・描画引数の記録を使う。

最終depth storeの省略は帯域削減の候補だが、今回はcolor/depth両方をStoreする既存の宣言契約を保ち、
scalarと同じ内容有効性のままbatch化の効果を比較した。代案の自動discardには最終consumer/exportの解析と
内容有効性の整合が必要になる。Storeを保つ分の帯域負担、とくに未測定のtile GPUでの影響は残り、
省略の採用は別の出力・同期・性能比較で判断する。APIが常にdepth Storeを要求するという意味ではない。

固定vertex/fragment shaderにはstorage副作用がなく、同じattachmentへのdepth/color操作は
[rasterization order](https://docs.vulkan.org/spec/latest/chapters/fragops.html#fragops-order)に従う。
この契約のdraw間へ旧scalarパス間barrierを機械的に残さず、全buffer/imageの必要な依存とlayout遷移を
描画区間の前へ、後続Compute/composite等への依存を区間の後へ置く。
Vulkan 1.3基準のまま、追加featureなしで区間内へ任意のbarrierを挿入しない。
任意shaderや異なるパスへこの統合条件を自動適用するものではない。

用途は資源keyの`BTreeMap`へ集約してstage/access/writeを合成し、同じbufferのVertex/Index両用途も保つ。
全drawのindexを各頂点範囲・Device上限で記録前に確認し、失敗はpassとdraw番号の文脈を伴って返す。
GraphWorkはdraw列・全物理資源・不変Programを完了まで保持し、pending中のasset更新・削除でも上書きしない。
clear/store・draw順・depth/tint・共有Vertex/Index・後半drawの失敗・pending所有と、
共有/異なる多数meshの比較結果を[現在の検証状況](../verification/current.md#draw-listの本実装と比較計測)へ記録する。

## 投入・完了・異常時の保持

`submit`と`wait`を分け、記録済み・未完了の所有者が命令から参照される全資源を保持する。
所有する対象をGPU実行中にCPUへ返さず、通常のアクセス・解放・再利用との競合を避ける。
投入前にpending状態を確立し、submit後のログ出力がunwindしても安全性を失わない。

正常完了として内容を返す条件は、対象Fenceの実際の成功待機とDevice activeの両方である。
timeoutは未完了として資源を保持し、再試行できる。一般的な待機エラーも完了証明にしない。
正常経路では対象Fenceだけを待ち、Queue／Device全体のidle待ちを使わない。

Graphのcanonicalな予定状態と、Fenceによる完了・回収は別に扱う。
完了観測には単調なserialだけを保持し、CPU-known入力を含む二つ目の`Snapshot`を保存しない。
これにより、後続の全面書き換えが正常にDrop経路で完了した場合も、不要な旧upload入力を
最後に明示waitした時点の状態として保持し続けない。Dropによる回収をCPU読み取り成功とはしない。

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

フレームループにはRuntimeがpendingを保持する`submit_frame`を追加した。
受理済みメタデータのDropは待機せず、`poll_frame`がtimeout 0で完了を観測する。
`wait_frame(timeout)`は完了済みの結果を先に調べ、必要なときだけ最古の未完了Fenceを待ち、
一件の`FrameCompletion`を返す。待機予算を各Fenceへ繰り返し適用せず、readback結果も破棄しない。
正常完了後もreadbackのCPU寿命は独立し、他の資源を先に再利用できる。
終了時の`drain_frames`は明示的な予算付き待機で、結果を破棄する。
Runtime自身のDropには既存の保守的な後始末を残すが、通常frameの進行には使わない。
停止やDevice lossを別操作で検出した場合も、各処理の実Fence結果を観測してから退役する。

この経路は既存Fence・所有leaseを活かし、timelineへの置換や別のframe poolを導入しない。
予定状態の引継ぎ、完了によるnative回収、CPU結果の保持を分けることで、
単一queueの正しい基準を保ったままPresentationや履歴の利用を追加する。

## 実行資源と共有stagingの再利用

Graphの物理資源cacheは、同じ`ResourceDesc`で登録解除済み・leaseなし・世代を更新可能なentryを
まず再利用する。一致がなければ`resource_capacity`まではentryを追加し、異なる寸法やbuffer sizeの
working setを保持する。上限時だけ、同じ再利用条件を満たすentryのうち最後の受理済み利用の
`scheduled.serial`が最小のものを置換する。同じserialならindexの小さいentryを選ぶ。
登録中、記録・pendingのleaseが残る資源、世代上限のentryは置換しない。候補がなければ
内部で待機せず容量不足を返す。失敗した準備を受理済み利用として新しく扱わない。

再利用候補はdescriptor別のindex集合と`(serial, index)`の退避順で索引化する。
登録中やlease保持中のentryを候補に残して全資源を毎回走査しない。
nativeの外部参照はleaseを通し、解放通知をentryごとに集約して取得時に候補へ戻す。
通知を消費した時点でも登録・世代・参照数を確認する。候補の選択だけでは旧entryを削除しない。

再利用は旧内容の初期化状態とCPU provenanceを消す一方、実際のlayout・access・writer scopeは保持する。
新しいnative資源とViewの作成・命名がすべて成功してから置換し、失敗時には既存entryのallocation・
世代・予定状態を維持する。OOM時に暗黙のevictionや再試行を加えない。

これは空きentryを直ちに異なる作成条件で置換する方式に比べ、交互の寸法でnative作成を繰り返す問題を
避けるための保持方針である。代わりにidle資源の保持量が増え、上限時の置換中には旧・新資源が一時的に
共存する。`resource_capacity`は保持するentryの件数上限であり、byte budgetや作成成功の保証ではない。
この物理cache自体にはbyte上限・memory budgetによるevictionを追加せず、`memory_stats()`のcache論理byteと
VMA Allocation／blockの実統計を区別して判断する。作成回数とCPU/GPU時間・メモリの比較は
[現在の検証状況](../verification/current.md)に記録し、件数削減だけからGPU高速化を主張しない。
上位のmesh常駐には別に件数・論理byte予算を設ける。常駐解除後のidle物理cacheやpending参照は
その常駐予算とは寿命が異なるため、この値をDeviceの総VRAM上限と説明しない。

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

### 大きなmeshの準備

`VulkanRuntime::create_buffer`は初期化範囲が空の永続bufferを明示登録する。
途中まで転送したbufferを全域初期化済みとしてexportする回避策は使わない。
通常のGraph uploadで登録bufferの範囲を更新し、受理後のfreshな内容snapshotから次のchunkを作る。
readや全域exportは必要範囲が初期化済みになるまでGraphの検査が拒否する。

engineのupload jobは固定staging容量・cache atom整列・未回収submission数の範囲で進む。
空きbyte照会は予約ではなく、断片化等による容量不足も通常のbackpressureとして扱う。
進捗offsetはsubmit受理後にだけ更新し、準備失敗や未投入OOMで未転送部分を済んだことにしない。
最終chunk受理後は同一queueへの描画依存で利用でき、完了FenceのCPU待機を必要としない。
取消・退避・CPU asset削除の後も、受理済みchunkが参照するstagingとnative資源を完了まで保持する。

## Windows Presentation

`for_window`は`Arc<winit::window::Window>`を受け取り、Runtimeと`Presenter`を返す。
Surface所有者がWindowとInstanceを保持し、Deviceと同じInstanceに由来することを保証する。
イベントループは利用側に置く。M3では確実なWindow寿命を優先した限定adapterとし、
他のwindow libraryへの接続はPresentation内の境界で追加できるよう、Graph・engine入力から分離する。
CPUのみの構築とheadless Vulkanはwindow依存を持たず、表示依存はWindows targetのoptional依存に限定する。

`Presenter::prepare`で再作成を進め、`extent`に合うGraph出力を作る。
Graphのlinear RGBA8_UNORM出力を`present`へ渡し、BGRA8_SRGBまたはRGBA8_SRGB、
SRGB_NONLINEARのSwapchainへfullscreen描画する。形式の違いを無変換copyで隠さない。
既存shaderのtexelFetchを使うためsourceとSwapchainのextentは一致を要求し、resize時は再構築する。
composite alphaはOPAQUE対応を要求する。straight RGBAをPRE_MULTIPLIED等へ無検証で流す
fallbackは置かず、透明Windowの合成方式をM3の機能へ広げない。
HDR、tone mapping、任意の出力形式・拡大縮小はこの経路に含めない。

Graph本体と表示は同じqueueへの別submitである。Graph解析にSwapchain所有を持ち込まず、
出力のcanonicalな予定状態からfragment samplingへ依存を記録する。
表示用submitがsourceの予定layout・stage/access・利用serialを進め、leaseを完了まで保持する。
読み取り専用の表示では内容revisionを変えず、Graph完了のCPU waitも挟まない。
同じ内容snapshotから次のconsumerを構築でき、同期は投入時のcanonicalな状態から計画する。
表示用submitが受理された後は、queuePresentが失敗しても予定同期状態は進み得る。
返値だけで旧同期状態へrollbackしたと考えず、実際に受理された状態を引き継ぐ。
一つのsubmitに融合する案より境界を小さくする選択で、submit追加のCPU費用・latencyの改善は主張しない。
GPU timestampのGraph区間には、この表示用submitとpresent待ちを含めない。

frame bookkeeping、実行枠、Swapchainのimage index、表示slotは別のものとして管理する。
表示slotはacquire用binary、present待機binary、present fence、キャンセル用Fenceとbindingを所有する。
EXT maintenance1を必須にしたため、画像ごとのbinary再利用に頼る拡張なしの方式ではなく、
各slotのpresent fenceと通常GPU参照の両方が終わったことを再利用の根拠にする。
present fenceのsignalは画面への表示完了そのものではない。
Swapchainの破棄には、その世代への全presentと通常GPU参照の終了が必要になる。

`present`はacquire前にRuntime実行枠を予約し、acquireはtimeout 0とする。
容量不足、`NotReady`、ゼロextentの`Suspended`で通常frameに暗黙waitを加えない。
実行枠はGraphと表示で共有するため、両submitの使用数を見た明示的なbackpressureが必要である。
`PresentationConfig::vsync`は既定trueでFIFO、falseでIMMEDIATEを要求する。
実Surfaceの対応モードを照会し、未対応は`Unsupported`として拒否する。
MAILBOXも垂直同期を待つため、OFF要求のfallbackには使わない。
これは最適化済みという判断ではなく、同期する／しないという利用者の選択を明示する契約である。
IMMEDIATEではtearingが起こり得る。OS compositorやdriver設定による実際の表示挙動は別に確認する。
[Presentモードの仕様](https://docs.vulkan.org/refpages/latest/refpages/source/VkPresentModeKHR.html)と
[Swapchain作成の契約](https://docs.vulkan.org/refpages/latest/refpages/source/VkSwapchainCreateInfoKHR.html)に従う。

`Presenter::set_vsync(bool)`は対応確認後に再作成を予約する。同値は再作成せず、
未対応の要求では設定と現在のSwapchainを保持する。`vsync()`は要求設定を返し、close後はNoneとなる。
`prepare`の成功した再作成で反映し、ゼロextent中は復帰まで保留する。
旧世代の進行中presentは元のモードのまま完了させる。resizeと同じbounded retirementを使い、
通常経路に全体idleを加えない。退役世代の容量不足時はpollして再試行する。
複数モードを同一Swapchainで動的に切り替える拡張経路は導入せず、
単一モードのSwapchainを再作成し、実画像数とSurfaceのusage・format・extent・上限を照会する。
固定3画像が最適であるとは扱わない。

`SUBOPTIMAL`の取得済み画像、未取得のtimeout、`OUT_OF_DATE`、Surface loss、Device lossを分ける。
acquire後の記録失敗やキャンセルではbinaryを待つ専用submitとFenceで消費を確認し、
未present画像をreleaseする。この例外的なキャンセル回収には明示した10秒のAPI待機予算がある。
release失敗の状態を推測して再releaseせず、証明できない所有を保持して停止する。
presentのOUT_OF_DATE／SURFACE_LOSTでは、表示要求が拒否されてもsemaphore待機等のqueue操作は
enqueue済みとして扱われる。OOMの状態不変保証と同じrollbackにしない。
公開outcomeもこれを区別し、`Presented`は表示要求の受理、`RecreateRequired`は再作成が必要な拒否、
`Recreated`は実際の再作成完了を表す。`Presented`を画面への表示完了の証明には使わない。
`RecreateRequired`後は`prepare`を進め、更新したextentに合わせて描画を準備する。

旧Swapchainは世代として保持し、`oldSwapchain`付き作成が失敗しても旧世代を再acquireしない。
保持世代数には上限があり、回収前の上限到達は容量不足として返す。
`poll`はGPU Fenceとpresent fenceを観測し、`wait_idle`／`close`はこのPresenterの処理だけを待つ。
Queue／Device全体のidleをpresent完了の証明には使わない。
正常終了はGraph frameを回収し、Presenterを明示closeしてからnative所有者を破棄する。
Dropの保守的回収でも完了を証明できない場合は、Window・Surface・同期・Device等の所有連鎖を保持する。

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

### Graphの任意計測

`set_profiling(true)`を明示した投入だけ、CPU prepare・record・submit・retireと
同一queue上のGraph全体のGPU timestampを採取する。buildとcompileは利用側で測る。
既定は無効で、通常submitにquery pool作成やCPU各phaseのtimerを追加しない。
無効へ戻しても、既に受理したsampleの所有・回収は変えない。

timestampは実queueの`timestampValidBits`と`timestampPeriod`を確認し、
上限付きquery pairを記録中・pendingのpayloadが保持する。
同じcommand bufferでquery reset、TOP_OF_PIPEの開始、Graph命令、BOTTOM_OF_PIPEの終了を記録し、
実Fence完了後に64-bit値とavailabilityをWAITなしで取得する。
未完了queryのresetや、結果取得のための毎frame待機は行わない。
値を結果へコピーしたらquery枠を返し、未読readbackに保持させない。

差分は有効bit幅のmoduloで扱う。submit前から回収までのCPU経過時間がcounterのwrap期間以上なら、
複数wrapを排除できないsampleとして未測定にする。これは保守的なRenderer側の測定方針である。
availability不足も時間ゼロとして扱わない。GPU時間はGraphの記録区間であり、
表示用submit・present/VSync待ち・画面への表示完了を含まない。
CPU retireはpayload整理、query取得、native実行枠のreset・返却を含み、Fenceでblockした時間を除く。
失敗したpollの反復費用はそのsampleに含めず、利用側のpoll全体時間と区別する。

`memory_stats()`は要求時にVMA統計を取得する。登録済み、pending等で保持中、再利用可能なcacheの
論理byte数、stagingの要求byte・整列予約byte・high-water、VMA Allocationとblockの件数・byte数を分ける。
画像の論理byte数はpacked texel量であり、optimal imageのAllocation sizeでもprocess VRAM使用量でもない。
このVMA統計はdriverやSwapchainの確保を含まない。
`EXT_memory_budget`は有効にしておらず、process budgetは未取得とする。
製品・性能用のLayer設定は明示的な`new_with_validation`で選ぶ。現行の既定Requiredは維持している。
要求したmodeとLayerから実際に観測した設定、未確認の情報を区別して記録する。

## 参照する仕様

- [キューとホスト同期](https://docs.vulkan.org/guide/latest/queues.html)
- [投入・状態・失敗時の保証](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html)
- [Lost Deviceの扱い](https://docs.vulkan.org/spec/latest/chapters/devsandqueues.html#devsandqueues-lost-device)
- [VMAのメモリmapping](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/memory_mapping.html)
- [VMA Virtual allocator](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/virtual_allocator.html)
- [Command資源の再利用](https://docs.vulkan.org/samples/latest/samples/performance/command_buffer_usage/README.html)

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
Windows表示は`vulkan,presentation`で追加する。`presentation`単独ではbackendを選ばない。
利用側のレビュー済みCompute shaderは静的descriptorのunsafe取込で扱う。未信頼shaderのsandboxや一般的なPassEncoderは提供しない。
Deviceで機能を有効にしていることと、その機能を使う処理が実装済みであることを区別する。

これは現行実装の境界であり、基盤の維持義務ではない。
[B0の基盤比較](implementation-policy.md#foundation-selection)ではnativeの低レベル所有・同期・退役・WSIを再利用し、
上位の固定Program/Operationを一般ownedパスへ置換する方針を採用した。
B1で資源用途・format・bindingを拡張するときも、同期・GPU寿命・native失敗等の必要な保証と移行時の検証を明示する。
以下は現在の実装済み契約である。レビュー済みRaster/Computeと、未信頼shader/encoderを区別する。

Graphのnative実行は次の責務に分ける。公開APIや汎用backend traitを増やす分割ではない。

| 内部module | 所有する責務 |
|---|---|
| `graph.rs` | 登録・世代・cache、import/export、投入の受理と公開状態 |
| `resources.rs` / `error.rs` | Graphに依存しない物理buffer/image/viewのVMA所有・破棄とnative実行エラー |
| `graph/work.rs` | 不変の準備operand、staging、必要な依存の計画 |
| `graph/work/record.rs` | native記録、投入受理・退役時のpayload処理 |
| `graph/work/diagnostics.rs` | `graph-diagnostics`だけで生成するlowering dump |
| `program.rs` / `program/creation.rs` | 管理下Programとattachment構成の不変pipeline、ABI/能力保証、作成と部分失敗RAII |
| `program/bindings.rs` / `program/record.rs` | inlineの容量付きdescriptor pool/cache/leaseと、分離したnative記録 |
| `presentation/` | native Surface/Swapchain/slot/acquire guard/Program/命令の所有・操作 |
| `graph/presentation/` | canonicalなsource状態とlease、Presenterの公開操作とruntimeの接続 |

記録の子moduleは準備operandへprivateにアクセスする。テストのためだけにfieldを公開せず、
資源の所有者を分散させない。記録から退役までのlease・実Fenceによる完了・失敗時の保持は同じ契約を使う。

## Instance・Device・キュー

InstanceとVMAが要求するAPIはVulkan 1.3とする。
初期化前にAPI、Layer、extension、物理デバイスの能力を照会する。
取得できた不足条件をまとめ、照会失敗で不明な条件を不足と決めつけない。
エラーには操作と元の原因を残す。
要求extensionはC文字列の内容で重複除去・整列してからFFI用pointerへ変換する。
同名でも保存場所が異なり得るため、pointerの数値を名前の同一性や順序に使わない。

物理デバイスにはAPI variant 0のVulkan 1.3以上、Graphics＋Compute対応キュー、
`timelineSemaphore`、`synchronization2`、`dynamicRendering`を要求する。
対応確認とDevice作成用のfeature構造体を分け、必要な3 featureを有効化する。
Indexed Indirect Drawでは、照会で対応した場合に `multiDrawIndirect` を追加で有効化する。
対応時は実Deviceの `maxDrawIndirectCount`、非対応時は1をコマンド数上限とする。
引数bufferにINDIRECT_BUFFER用途を持たせ、Computeの書き込みからDRAW_INDIRECT／INDIRECT_COMMAND_READへ同期する。
初期化で取得した不変な`VkPhysicalDeviceLimits`をDevice所有者に保持し、画像/Program作成でも再利用する。
実際のformat/type/usage/extentの対応確認とnative作成失敗は別に処理する。

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
製品構成は既定feature空からbackendだけを選び、Validation Layersと診断拡張へ依存しない。
最適化と独立した`development-validation`は、Graphの追加再検査と`vulkan-diagnostics`を有効にする。

`development-validation`ありの既定`ValidationMode::Required`はValidation Layer、debug_utils、layer_settingsを要求し、
core validationを維持したままSynchronization Validationを追加するよう指定する。
受入試験ではレイヤー起動情報を確認し、外部設定で検証を無効にした成功を受け入れない。

上記featureなしの既定は`ValidationMode::Disabled`で、Layerとlayer_settingsを要求しない。
`vulkan-diagnostics`は単独でも選択でき、native Layer要求とは別にdebug_utils・命名・label・monitorを追加する。
offではそれらの専用構築・保持をコンパイル除外し、明示`Required`は対応できない要求として拒否する。
releaseかどうかでこの選択を変えない。外部loader設定によるLayer注入は別の確認事項であり、
診断を組み込まない実行から「Layerが存在しなかった」と断定しない。
Graph入力の保証・同期・寿命・容量・native失敗は維持する。
[適用と残る保証処理](../guides/validation-policy.md#current)を参照する。

`tracing`のsubscriber・出力先・表示レベルは利用側が設定する。
ライブラリはglobal subscriberを設置せず、初期化時の`Dispatch`を保持して別スレッドのcallbackにも使う。
GENERAL／VALIDATION／PERFORMANCEの全重要度を受信し、ログフィルターとは独立してwarning/errorとcallback失敗を数える。
最初のwarning／error 128件を保持し、保持できなかった問題メッセージの件数も示す。
直近64件の全severity履歴と総受信数はprivateな受入観測用としてtest構成だけで保持する。

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

standaloneなwhole-buffer/image transferとbatchは現在test/性能参照経路である。
その専用資源型・初期化metadata・planは通常libraryへコンパイルしない。
本番Graphの物理資源、共有staging、実行pool、退役・readback所有は引き続き通常経路に残す。
参照batchでもprivate builderの保証済みleaseを再走査する独立診断だけを開発構成へ分け、
正常なFence完了後に初期化範囲のunionを一度移す。native記録後には不要なcopy operandを解放する。

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

論理Graphは`ImageDesc::mip_levels`/`array_layers`とsubresource ViewをCPUで計画する。
nativeは2D・layer/sample各1を扱い、B2のsampled入力とwhole-chain uploadは複数mipを許可する。
owned attachment passとRasterはmip数1の画像で、
color一個（RGBA8/RGBA16F）とoptional D32、またはdepth-onlyをDynamic Renderingへlowerする。
Load/Clear/DiscardとStore/Discardを保持する。MRT、複数layer、multi-mip画像のattachment/storage等、
未対応なlive構成は記録・投入前に拒否する。cache keyはbase extent/formatとmip数を区別する。
culledな未対応枝は取得しない。未信頼shaderや一般的なarray/MSAA実行は扱わない。

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
固定usage組を持つ。RGBA16Fはtransfer source／destination、storage、sampled、color attachment、
D32_SFLOATはdepth attachment、sampled、transfer source／destinationを持つ。
public GraphのD32 uploadは受け付けず、ここでのusageは実装した同期・転送と再利用の能力条件を表す。
`Rgba8Srgb`は`R8G8B8A8_SRGB`のsampledとtransfer source／destinationを使い、
storage/color attachment用途へは受け付けない。色のdecodeはfilterの前にnativeで行い、alphaは変換しない。
いずれも2D・optimal tiling・layer／sample各1とし、実際のusage組の能力とextent/mip数、
framebuffer／viewport上限を照会する。Graph画像はallocationとImageViewを一緒に保持する。
固定usageはexport後に対応操作へ再importできるようにするためであり、
usage削減による最適化や、未確認GPUでの対応を主張しない。

B2のmip chain uploadはmip0から順のpacked texel列を受け、各levelのextent・byte offsetを
`VkBufferImageCopy`へ写像する。全chainのbyte数を照合し、copyした全mipを初期化する。
画像copy/readbackはmip数1に限定する。画像単位のstate/layoutを保持し、barrierの範囲も全chainとする。
部分native更新やsubresourceごとの独立状態を実装したという意味ではない。
sampled Viewは準備時に全chainと各単一mipを作成し、任意のbaseのcount1、またはbase0の全chainを選べる。
一部だけの複数mip範囲は未対応として拒否し、Viewを作るnative呼出しを通常frameへ遅延しない。
Graphは選択範囲の全mipをread footprintへ含め、未初期化levelをfilter設定で隠さない。
linear filteringに必要な実format featureも確認する。

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

固定の画像変換Computeは8×8のlocal size、set 0のbinding 0／1に各1個のRGBA8 storage imageを使う。
push constantはoffset 0のu32 1個で変換を選ぶ。端数のinvocationはshader内で除外し、
hostでもworkgroup size・invocation数・dispatch数・descriptor・push constantの上限を確認する。
固定の画像合成はset 0 binding 0のcombined image samplerから同じ座標を`texelFetch`し、
別の同寸画像へfullscreen triangleで書く。filterやsRGB変換はしない。

pipeline・layout・samplerは明示したworking setの準備でCompute／Raster／Compositeの必要な種類を作成し、
成功した不変のProgramをruntimeで保持する。buffer-onlyなら画像用Programを要求せず、
RGBA8 classでCompute/Composite、RGBA8とD32の組でRasterを準備する。
RasterのD32 depth対応を含む形式・ABI上限を、その種類のnative作成前に検査する。
通常frame中に新しい種類を遅延作成しない。種類や容量の追加は、既存consumerを返却した準備境界で行う。
作成に失敗したProgramをキャッシュせず、部分作成分を解放して再試行可能な状態を維持する。
descriptor arenaは同時実行数とGraph内binding数に上限を持ち、
必要なlayoutとsetを準備境界でまとめて作成する。GPU完了または未投入キャンセルを確認してから
既存setの内容を更新して再利用し、通常frameでsetのallocate/freeやpool resetによる暗黙freeを行わない。
容量・cache・lease操作は`program/bindings.rs`のinline BindingPoolへ閉じる。
leaseは`Arc<ShaderPrograms>`を保持し、最後のownerのDropはcached descriptor poolを
不変Program/layoutより先に解放する。module分離によるallocation・Arc・lockの追加はない。
更新済みdescriptorを参照する記録済み／未完了命令を、別の更新で無効にしない。
pipeline作成に失敗した際も、部分的に作成できたhandleを解放する。

頂点はmodel座標xyz（入力W=1）とlinear rgbaで、描画ごとのMVPでclip座標へ変換する。
`RasterDraw.parameters`へ`RasterParameters::default().into()`を指定するとidentity MVPと白tintを使う。
頂点shader用push constantはcolumn-major mat4がoffset 0、linear tint vec4がoffset 64の計80 byte。
実Deviceの上限を照会し、pipeline layoutと記録のstage・offset・sizeを一致させる。
色と行列はGraphの不変operandとして保持し、後続frameのCPU入力変更に依存しない。
深度範囲は0〜1の通常Z、
clear depthは1、通常meshのcompareはLESS＋depth write、viewportの高さは正、cullingは無効とする。
標準prepass後の`mesh_prepassed`／`mesh_hdr_prepassed`はLESS_OR_EQUAL＋depth writeなしであり、
同じgeometry・View・変換で作ったD32をLoadする。Graphはdepth必須・Load必須を0 drawでも確認する。
Program descriptorへこのstateを含め、両backendが同じ契約を使う。
front faceはCOUNTER_CLOCKWISEを設定するが、cullingで判定を使う段階では向きも再検証する。
この標準mesh経路は汎用projection builder、照明、tone mappingを提供しない。
色はlinear RGBA8_UNORMまたはRGBA16Fで、標準meshはalphaを保持してblendingを無効にする。
独自Programは同じdescriptorでReplace／straight alpha／premultiplied alpha／加算と
depth compare／write／attachment必須条件を指定できる。blendを使う形式は
`COLOR_ATTACHMENT_BLEND`対応もpipeline準備時に確認する。
u32 indexは頂点bufferの範囲と、起動時に保持した実際の`maxDrawIndexedIndexValue`の両方を検査する。
Vulkan 1.3の全adapterで32bit全域を使えるとは仮定せず、feature有効化だけで上限を推定しない。

明示した`ManagedRasterProgram::mesh()`のowned Raster列は一つのDynamic Rendering区間で宣言したcolor/depthのLoad/Storeを適用し、
pipelineとviewportを設定してから、owned draw列の順にbuffer binding・inline引数・indexed drawを記録する。
宣言したdrawごとにインデックス範囲とインスタンス数を渡し、描画順を維持する。
空listにはclear-onlyの既存経路を使い、Raster Programの作成を要求しない。
単一drawも同じ列で記録する。Engineの標準opaqueは従来どおりClear/Storeを明示する。
ProgramのSPIR-Vは管理下manifestからfactoryへ渡し、native pipelineは準備済み`ShaderPrograms`が保持する。
CPUのstatic Program identityだけでnative所有者を代替せず、WorkのProgram/資源保持とplan pinをGPU完了まで維持する。
color-only、depth-only、RGBA16Fとdescriptor-bound Programも同じowned列を使う。
MRTは最大8色と任意のD32を一つのDynamic Rendering区間に設定し、Program切り替えも同じ区間で記録する。
色形式と個数はDevice上限と照合し、共通のblend状態を各カラー出力に使う。
複数subresourceを覆うattachment／storage Viewやarray等の未対応live計画は物理取得前に拒否する。
sampled Viewの単一mip／全2D mip chainは対応範囲に含む。

`prepare_raster_programs(&[ManagedRasterProgram])`は`prepare_working_set`に続く明示準備境界である。
選択したtrusted Program（prepassedの深度stateも含む）とcolor形式・depth有無をkeyとしてpipelineを保持し、
通常prepareで新しい種類を作成しない。既存meshのcolor＋depthは従来のworking-set準備で利用できる。
HDRは同じfloat出力shaderをRGBA16Fへ書く。depth-onlyはcolor attachmentを持たず、
fragment stageなし、またはalpha判定とdiscard用fragment stageを許可する。
stageの有無は実bytecodeから決め、fragment depth/stencil/sample-mask出力やstorage副作用は許可しない。
pipeline layoutは各descriptorの頂点属性、最大128-byteのVS／FS inline定数、
最大4 groupのUniform／StorageRead／sampled image・separate samplerを具体化する。
standard meshの80-byte定数とtexturedのbindingも同じ経路を使う。
Uniformの実Device alignment/range上限は記録前のnative適合境界で保証する。
同じWork内では、Graphの不変binding範囲とnative Raster layoutが一致するdrawでdescriptor setを共有する。
準備済みの索引から結果を取得し、最初の使用だけでsampled適合とbufferのDevice上限を保証してsetを設定する。
同じ内容をdrawごとに再検査・再更新せず、異なるbinding/layoutは別の予約済みslotを使う。
再利用索引とsetはWorkのlease内だけで有効で、取消または実GPU完了後にresetする。
異なるWork/pending間のpointer一致を再利用根拠にせず、記録済み／pendingのsetを更新しない。
索引はbinding容量に対応する旧bool配列から、二つのdepth variantとnative layout／set結果を持つ
`RasterBindingEntry`配列へ変わる。その`size_of`×binding上限を各Workへ予約する交換条件があり、
draw別のset複製・更新とDevice適合の再検査を減らすために採用した。全域的なcacheや通常frameでの増設は行わない。
Program、pool/layout、View、samplerと画像/bufferはWork/leaseの所有へ含め、cancelまたは実GPU完了後だけ再利用する。

static `RasterProgramDescriptor`の取込は明示したunsafe境界であり、binaryの有効性だけでなく
全許容定数・入力に対する範囲安全、宣言外アクセス／storage副作用の不在を呼出側が保証する。
通常Graphはdraw定数のbyte数と資源範囲・初期化を保証する。genericなword列にfiniteの制約は付けず、
`RasterParameters`の有限値保証をその便利変換以外へ一般化しない。
shaderのbytecode／metadata／固定状態を含む不変descriptor identityでlookupし、
runtime生涯の追加回数ではなく、明示準備した現在の集合に対して64資産の上限を適用する。
集合変更は新しいProgram所有者とdescriptor領域を準備してから切り替えるため、
失敗時は旧集合を利用でき、通常frameのlookupに可変Vecの再確保やlockを加えない。
この変更境界のpipeline再作成費用は通常frameメモリ保証と区別し、時間性能の改善を主張しない。

RGBA16Fのpacked転送は1 texel8 byteであり、共有stagingのoffset alignmentをatom sizeと8 byte以上にする。
D32 readbackはDEPTH aspectとTRANSFER_SRC_OPTIMALを使う。画像copyは同extent/formatのRGBA8/RGBA16Fを扱い、
固定ComputeのRGBA8契約とは分ける。各実format/usage能力の作成時照会とnative失敗処理を維持する。

公開`standard::passes`と`standard::graphs`は通常のGraph／Program／copy APIだけで構築する。
利用側の多段Computeも同じ公開`dispatch`で接続し、標準専用のloweringやnative実行分岐を追加しない。
三経路の混合構成ではRaster／Compute双方のProgram、画像class、vertex/indexとprovenanceを明示準備し、
Graph由来のattachment→sampled／storage→copy依存、実Fence完了とCPU結果返却までの所有を維持する。
実GPUのstrict／shippingと全frameメモリの実行範囲は[検証記録](../verification/current.md#公開三経路のconsumer移行と混合compute受入)に記す。
Engineの便利経路も公開forward recipeを呼び、native側にopaque等の効果分岐を増やさない。
prepassedのshader深度writeを止めてもGraphのattachment accessは保守的read/writeのままとし、
通常のEARLY/LATE_FRAGMENT_TESTS依存とrendering scope外のbarrierを維持する。
標準outputは画像copyであり、RGBA8／RGBA16Fの値を同descriptionの別画像へ保存する。
部品やpost境界がexport/readback/submitを暗黙に追加することはない。

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

用途は論理資源indexの準備済み配列へ集約してstage/access/writeを合成し、
触れたindexを予約済み配列で列挙する。同じbufferのVertex/Index両用途も保つ。
全drawのindex保証を各頂点範囲・Device上限で記録前に確認し、失敗はpassとdraw番号の文脈を伴って返す。
管理下meshは取込時に確立した不変count／最大値を使い、index本文や検査用CPU byte列を再保持しない。
任意Graph入力では部分byte更新にも必要な内容追跡を維持し、同countの最大値要約を再利用する。
保証の継承・失効・未知内容の拒否は[Graphの内容契約](rendergraph.md#producer範囲attachment)に従う。
GraphWorkはdraw列・全物理資源・不変Programを完了まで保持し、pending中のasset更新・削除でも上書きしない。
clear/store・draw順・depth/tint・共有Vertex/Index・後半drawの失敗・pending所有と、
共有/異なる多数meshの比較結果を[現在の検証状況](../verification/current.md#draw-listの本実装と比較計測)へ記録する。

<a id="managed-uniform"></a>

### 管理下Programと資源binding

`renderer-7ft.2.1.2`は、共通の`program::ManagedProgram`からnative Programを作成し、
UniformとStorageの資源・binding作成を共通APIへまとめる。Graph非依存の`managed`
moduleがpipeline、layout、allocation、bindingの所有を担当し、既存の固定`ProgramKind`を増やさない。
`.2.1.4`でRGBA8画像/View/samplerの作成とbindingを加え、`.2.1.5`でRGBA16Fへ拡張する。
そのB1 fixtureの初期範囲に、他format/mip/layer/filterと未信頼shaderの取込は含めない。
B2で加えたsRGB・mip chain・linear repeatの現行契約は[画像とcopyの範囲](#画像とcopyの範囲)に従う。
`Graph::dispatch`は管理下buffer・sampled assetの論理CPU計画を接続する。各drawの型付きbindingと共に、manifest由来のVertex/Fragment/Compute stageと実footprintを保持する。`textured_mesh()`のVS UniformとFS sampled画像/samplerはowned Rasterのnative PSOへ接続する。
owned dispatchは論理Graphの資源とbindingを、明示準備したCompute pipeline/descriptorへ具体化する。
Graph外のnative Bindingsをimportする経路とは別であり、既存の実行枠・資源lease・Fence退役を使う。
culledな未対応枝は除去できる。Uniform/storage/sampled用途は保持した実shader stageと対応するaccessへ具体化し、Vertex/Index fetchはそれぞれ入力stageを使うが、
storage画像はCOMPUTE_SHADERとSHADER_STORAGE_READ/WRITE、GENERAL layoutを使い、
sampled用途との遷移とpass間依存を保持する。

safeなshader入口はレビュー済みの`ManagedProgram::uniform_readback()`、`storage_readback()`、
`sampled_readback()`、`sampled_float_readback()`である。
前者は以下のbuffer ABIを持つ。
GLSL 450・SPIR-V 1.3、entry `main`、local size 1×1×1、set 0/binding 0の
`std140 uvec4`（offset 0、16 byte）を読み、Storageのset 1/binding 0へ
16 byteを書く。一つのinvocationだけを実行し、値依存の範囲外アクセスや競合を作らない。
reflection・hashだけで任意source/binaryを信頼済みへ昇格させる機能は提供しない。
利用側は`ComputeProgramDescriptor`をstaticに保持し、`ManagedProgram::from_trusted_descriptor`のunsafe境界で
Compute shaderを追加できる。binary・entry・local size・binding ABIとformat、buffer footprint、
全域writeとdispatch/extentの関係、宣言外アクセス・範囲外アクセス・race・shader内同期の安全性を
取込側がレビューする。バイナリや宣言を変えるとその保証は失効し、新しい不変descriptorを使う。
反射情報だけではデータ依存アクセスや全域writeを証明できないため、任意binaryを受けるsafe入口は採用しない。
この責任分担はnativeの実Device能力・alignment・上限・容量・失敗処理を省く理由にはしない。
現行descriptorはentry `main`、非配列・非MSのfloat 2D画像と通常FP32演算、D3D12のgroup=register space／
binding=register番号を前提にする。bufferはroot descriptorで扱えるraw/structured ABIとし、
typed buffer、push constant、descriptor配列、Workgroup/shared storageや未有効のoptional featureは扱わない。
利用側fixtureのshaderと二API生成物も固定toolで管理し、Rendererのeffect列挙を増やさず実証する。
生成時にSPIR-VとDXILのreflectionをABI契約へ照合する。CPU試験でRustのmanifest表も
生成reflectionへ結び付け、kind/group/binding/minimum byte数を一致させる。

作成時に実Deviceのcompute/descriptor上限とABIを検査し、manifest表からgroupごとの
set layoutを作る。`create_managed_buffer`はUniformとStorageを受け、既存Graph registryの
`create_buffer`とは所有の役割が異なる。Uniformは`UNIFORM_BUFFER`のVMA allocationへ
初期byteを一度write/flushし、公開後のCPU上書きを許可しない。Storageは
`STORAGE_BUFFER|TRANSFER_SRC`、`AutoPreferDevice`で未初期化作成し、CPU mapを公開しない。
`std140`を使い、UBO標準layoutやscalar layout featureを要求しない。

`create_bindings`は全slotの過不足・重複、用途、owner、論理範囲を検査し、uniform/storage
それぞれの実Device offset alignmentと最大rangeを満たすことを保証する。入力順は自由で、
成功後はmanifest順のProgram・buffer/範囲・View/Image・samplerを強保持する。pool/setを新規作成し一度だけupdateする。
slot解決は呼出し元の入力を直接参照し、検査専用descriptor Vecを作らない。各manifest slotへ
対応する入力がちょうど1件であることと全体の件数から、過不足・重複を排除する。
全結果の解決・資源検査が成功してからnative pool/setを作る。選択済みpairのslotを再照合せず、
同じslotの対応付けを別処理で再探索しない。保持用buffer/範囲配列とnative descriptor更新用配列は維持する。
Uniform専用の作成API・型とconsumerは共通APIへ置換し、互換wrapperを残さない。
一資源の用途をUniformかStorageに固定し、両用途への同一資源aliasを許可しない。
不変なのはProgram/descriptor/資源の版と範囲であり、Storage本文はGPUが書き換えられる。
作成成功や`StorageWrite`だけで初期化済みにはしない。このshaderが確立するのは指定offsetの16 byteだけである。
失効操作を公開しないABI・範囲を記録時に再走査せず、native失敗、容量、同期、実GPU完了と所有は製品に残す。

受入は公開APIで作成したStorageを使い、命令記録とreadbackだけを`cfg(test)`へ限定する。
fixture専用の排他leaseで同一Storageへの重複記録を拒否し、cancelまたは実Fence完了で返す。
reuse時のshader write/copy readから次のshader writeへの依存も記録する。
leaseは既存transfer payloadと一緒に保持し、不確定失敗でも早期に返さない。
この試験用state/lease/入力は通常buildに含めない。一般Graphは別の製品用計画・状態・所有へ接続し、
private fixtureの入力照合やCPU待機で同期を補わない。

`prepare_compute_programs(&[ManagedProgram])`は明示準備境界でpipeline/layoutと容量付きdescriptorを準備する。
通常frameのprepare/recordはその容量を借用し、未準備Programや枠不足を型付きエラーにする。
記録済み未投入、pending、完了不明の処理が参照するpipeline/descriptor/Viewを早期に返却しない。
RGBA16F storage画像は実format/type/usage組の能力を確認する。format qualifier付きのshaderを使い、
read/write-without-formatのfeatureを暗黙に要求しない。SPIR-Vの能力条件を生成物と有効featureへ対応付ける。
実行範囲は[現在の検証状況](../verification/current.md#managed-uniform)へ記録する。

`.2.1.7`の`StorageRead`は既存Storage資源を読むbindingであり、別のbuffer用途を増やさない。
Vulkanのdescriptorはread/writeとも`STORAGE_BUFFER`で、descriptor上限も両者を合算する。
offset alignmentと最大rangeは既存storage契約を使う。`storage_read.comp`はset0/binding0の
readonly `std430 uvec4`の先頭16 byteを読み、set1/binding0のwriteonly出力へ16 byteを書く。
生成reflectionでreadonly/writeonlyとmember/slot/sizeを照合し、不変manifestへ結び付ける。
binding作成成功をStorage本文の初期化保証にはしない。

private受入は同じWorkの中で`uniform_readback`をproducer、`storage_readback`をconsumerとして実行する。
準備時にレビュー済みassetのidentity、producer出力とconsumer入力の同じBuffer版・先頭offset、
実際の16-byte write/read保証を結び付け、input/outputの全buffer aliasを拒否する。
公開binding作成へalias検査用scanを増やさない。両Program/Bindingsとinput/outputを保持し、
各物理Storageを一度だけ排他leaseする。producer write→consumer readの
`COMPUTE_SHADER`/`SHADER_STORAGE_WRITE`→`COMPUTE_SHADER`/`SHADER_STORAGE_READ`依存を置き、
consumer出力だけを最後にcopy/readbackする。間のCPU待機やreadbackで同期を補わない。
再利用時は前consumer read→次producer writeの実行順も保つ。
asset identity確認とproducer/canonical slotの試験用metadataは`cfg(test)`に限定し、
製品のcreation契約と一般Graphの実行計画を混同しない。

`create_managed_image(ImageDescriptor)`はRGBA8_UNORM/sRGB、RGBA16FまたはD32_FLOATの2D、layer/sample各1、
optimal tilingで作る。色は`SAMPLED|TRANSFER_DST`、D32は`DEPTH_STENCIL_ATTACHMENT|SAMPLED`に固定する。Graph画像のstorage/color attachment等の
固定usageやframebuffer/viewport上限を持ち込まず、実際のformat/type/tiling/usage/flagsと
extent/mip/layer/sample対応を作成境界で照会する。packed pixel量をVMA allocation sizeへ代用しない。
作成直後は未初期化であり、upload/submit/waitを暗黙に行わない。
`preferred_depth_clear: Option<f32>`はD32の後続attachment clearに予定する値であり、
内容初期化や後続clear値の制限を意味しない。`None`も許可し、色への`Some`、非有限値、
[0,1]外、subnormalは作成境界で拒否する。この不変metadataをView/Imageの所有とともに保持する。
`ImageDescriptor::mip_levels`は非ゼロでextentから許される段数以下とする。
`create_image_view`は選択する連続mip範囲、ownerと`ImageViewUsage`を検査し、ViewがImageを保持する。
`Sampled`が既定で、`DepthAttachment`はD32だけに許可する。aspectはImageのformatからcolor/depthへ導出し、
呼出し側に別のaspect指定を要求しない。depth attachment Viewのmip countは1、sampled bindingには`Sampled` Viewだけを受け付ける。
`create_sampler(NearestClamp)`はnormalized coordinate、nearest min/mag/mip、clamp-to-edge、
LOD0、`LinearRepeat`はlinear min/mag/mip、repeat、選択View内のLOD範囲を使い、いずれもcompare/anisotropyなしで作る。
`LinearClamp`は全軸clamp、`LinearRepeatUClampV`はU repeat/VとW clampで同じlinear設定を使う。
Graphのsampler operandも実際のnative samplerへ対応させる。Device全体の生存sampler数をnative作成前に予約し、
失敗時またはnative破棄後に返す。既存固定Graph/表示samplerも同じ上限契約を使う。

`Binding`のslotと`BindingResource`のbuffer/View/sampler参照へ入力をまとめ、旧buffer専用
binding APIを置換する。sampled shaderはset0/binding0のseparate sampled image、
set0/binding1のseparate sampler、set1/binding0の16-byte StorageWriteを持つ。
全slot解決と種別/owner/範囲検査後に通常descriptor setを一度作成/updateし、公開後は変更しない。
View/Image/samplerをdrawごとに再走査する処理や、検査専用Vecを追加しない。
Deviceに対するsampled-image/samplerのstage別/set別descriptor上限もProgram作成時に保証する。
Rasterのtrusted `sampling_pairs`は実際の画像・sampler組を保証し、対応samplerがlinearの場合だけ
その画像のfilter能力を照合する。色のlinearとD32のnearestを同じパスで使っても、D32へlinear能力を要求しない。
同じ不変binding集合はsubmit内で一度だけ確認し、明示準備したビット領域を実行枠の再利用時に戻す。
追加のヒープ確保やdrawごとの内容再走査は行わない。Computeは従来の保守的な能力条件を維持する。

private受入は2x2の非対称RGBA8をuploadし、4回の`textureLod(..., 0)`を実行して
`packUnorm4x8`で16 byteへ戻す。UVは(-0.25,-0.25)、(0.6,0.2)、(0.2,0.6)、(1.25,1.25)で、
nearestとclamp、行/成分の対応を実内容で確認する。`texelFetch`への置換でsamplerを除去しない。
fixtureはImageとStorageの排他leaseを記録から退役まで保持する。画像は初回UNDEFINEDからTRANSFER_DSTへ遷移し、copy write→compute sampled readを
可視化する。初回は既存の保守的COPY execution依存も維持する。一度uploadした画像は
再初期化せずSHADER_READ_ONLY_OPTIMALで再sampleし、新しい色は別画像版へuploadする。受理済みsubmitの予定状態と
未投入cancelを区別し、GPU完了を確認できない失敗では資源/descriptor/lease連鎖を早期解放しない。
画像の公開作成/所有/descriptorは製品へ残し、upload色・private executorのstate/leaseは`cfg(test)`に限る。

RGBA16Fは`ImageFormat::Rgba16Float`を`R16G16B16A16_SFLOAT`へ対応づけ、
同じ実format/usage照会とfull color Viewの所有を使う。Viewのnative formatは保持したImageから決める。
RGBA8の対応や既存Graph画像の用途を、RGBA16Fの対応根拠へ流用しない。
`sampled_float_readback`は同じ4座標の各RGBAをFP32でsampleし、`floatBitsToUint`で
4個の`uvec4`、計64 byteをStorageへ書く。画像の16-bit格納からFP32への解釈を確認するもので、
shaderの16-bit演算型や追加の`shaderFloat16` featureは使わない。
これは[画像readのbinary32への変換](https://docs.vulkan.org/spec/latest/chapters/images.html#images-reads-numericformat)と
[shader内16-bit型のfeature](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceShaderFloat16Int8Features.html)を区別した選択である。
private受入は32 byteの2x2 half入力と64 byteの出力を区別し、負値・小数・1超の値と行/成分順を照合する。
shader ABI・画像format・参照範囲は作成時に保証し、upload byte列とreadback範囲の変更は試験専用経路に限定する。

共通の`renderer::ImageFormat::Depth32Float`は`D32_SFLOAT`へ対応する。attachmentとsampledの組合せを
実Deviceへ照会し、depth-only aspectのattachment用・sampled用Viewが同じImageを保持する。
private受入は全2x2領域のdepth attachmentをDynamic Renderingのload `CLEAR`/store `STORE`で
0.25または0.75へ初期化する。clearの前後でlayoutを明示し、rendering scopeの外に
`EARLY_FRAGMENT_TESTS|LATE_FRAGMENT_TESTS`/`DEPTH_STENCIL_ATTACHMENT_WRITE`から
`COMPUTE_SHADER`/`SHADER_SAMPLED_READ`への依存を置く。
`DEPTH_STENCIL_ATTACHMENT_OPTIMAL`から`SHADER_READ_ONLY_OPTIMAL`を使い、
separate depth/stencil layout featureを追加しない。
初回UNDEFINED遷移には、既存Graphのdepthを含む画像と同じCOPY execution対策を継承する。
これは既存の保守的な正しさ対策であり、D32の性能改善やdriver原因の確定を主張しない。
[attachmentのload/storeとlayout](https://docs.vulkan.org/refpages/latest/refpages/source/VkRenderingAttachmentInfo.html)と
[画像barrier](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageMemoryBarrier2.html)に従う。
既存`sampled_float_readback`と非comparison nearest samplerを再利用し、64-byte出力の4 texelのR成分を
独立なFP32期待bit列へ照合する。色用uploadをdepth clearへ流用せず、D32にtransfer用途を追加しない。
初期化後の旧版reuseは再clearしない。attachment Viewも記録/pendingの所有へ含め、同じImageの排他lease、
cancel、受理済み予定状態、実Fence退役、最終解放は既存fixtureの契約を引き継ぐ。
公開の用途/format/ownerとpreferred clear値の保証は製品に残し、clear命令記録・試験入力/stateは`cfg(test)`に限定する。


## 投入・完了・異常時の保持

`.2.5`では既存のcanonical資源・内容revision・native状態計画を一般owned Compute bindingの
Graph間受け渡しにも適用した。HDR StorageRead／Writeと任意buffer bindingを使う履歴chainを、
producerのCPU完了待機なしに投入する。初回／reset／resize、pending中の更新・削除と古いsnapshot拒否を
D3D12と同じ利用側定義で確認する。実GPU完了までのPhysical/View/descriptor保持を維持し、
今回新しい資源寿命経路やVMA設定は追加していない。runtimeのowner identityだけをGraphと共有する
process共通採番へ移し、backend間のkey衝突を防いだ。通常frameのメモリ測定と未実行範囲は
[現在の検証](../verification/current.md)を参照する。

公開import snapshotの発行は共通のprivate-field型とchecked constructorへ集約する。
新規buffer登録、canonical予定内容のrefresh、受理予定exportでdescriptor／初期化範囲を保証し、
安定snapshotのcloneとGraphへの反復importでは同じ内部内容を再点検しない。
画像全体の初期化はcanonical範囲から全mip×layerの被覆として導出し、boolの自己申告を受けない。
詳細な保証と失効は[Graphのsnapshot契約](rendergraph.md#runtime-import-snapshots)に従い、
投入境界のowner・世代・revision、可視化・所有・実GPU完了の扱いは維持する。

同じqueueでwriterをreadへ可視化した後、write/layout変更がなく、直近に確立したreadの
stage/access pairが完全に同じならwriter→reader barrierを重複記録しない。bufferと画像の両方へ適用する。
このexact pairを最後のwriterと累積reader scopeから分け、Snapshotごとに16 byte追加する。
writeでpairを失効し、異なるreadでは必要なwriter依存を設けてpairを置換する。
後続write/layout変更は累積readerとwriterを使い、pair一致だけでWAR/WAWを省略しない。
presentationも同じ更新を使い、layout不変の再presentで過去readerのscopeを失わない。
累積maskから任意のstage/accessの組合せの可視性を推論しない。一般Programへの拡張時は再検証する。
根拠は[`vkCmdPipelineBarrier2`の同一queueの後続命令への第2scope](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdPipelineBarrier2.html)である。

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

この経路は既存Fence・所有leaseを活かし、timelineへ置換しない。
作業・CPU結果・exportメタデータには、それぞれの寿命と上限に対応した準備済みpoolを持つ。
予定状態の引継ぎ、完了によるnative回収、CPU結果の保持を分けることで、
単一queueの正しい基準を保ったままPresentationや履歴の利用を追加する。

## 実行資源と共有stagingの再利用

`prepare_working_set(FrameCapacity, &[ResourceClass])`を初期化・資源変更境界で呼び、
Graphの配列上限とwork/result/export枠、import範囲snapshot、CPU provenanceの容量を準備する。
`FrameCapacity::index_prefixes_per_resource`は内容ごとの異なるindex-count要約数を予約する。
byte provenance区間数の`provenance_segments_per_resource`と独立し、いずれも通常frameで増設しない。
prefix容量不足は要約の追い出しではなく型付きエラーを返し、同じ内容のA→B→Aで本文を再走査しない。
`ResourceClass`はbackend非依存のbuffer/image descriptorと同時保持数であり、登録済み入力とは別に
記録・GPU・exportが保持し得る未登録instance数を含める。変更には既存のframe/result/metadata/range tokenの返却が必要である。
入力やdraw/pass構成は準備済み上限内で毎frame変更でき、固定planの再生を条件にしない。

workのresource-indexed配列とSnapshot backingは上限全体を事前準備するが、通常のsubmit、
完了観測、返却時のresetとprovenance mask copyは当該Graphの宣言resource prefixだけを辿る。
このprefix長は最初のlease取得前に設定し、途中失敗・取消でも取得済み参照と状態を全て返す。
次のGraphが小さくても前回のprefixは返却時にreset済みで、再び大きいGraphを作ったときに旧状態を復活させない。
準備時の容量と常駐量は減らさず、予約上限だけに比例する通常frameの走査を除去する。

通常Graphは同じ`ResourceDesc`で登録解除済み・leaseなし・世代を更新可能なentryだけを再利用し、
一致がなければ容量不足を返す。native作成・置換・待機を暗黙に行わない。
明示した準備と`create_buffer`等の資源作成境界では、`resource_capacity`まではentryを追加して
異なる寸法やbuffer sizeを保持できる。その上限時だけ、同じ再利用条件を満たすentryのうち最後の受理済み利用の
`scheduled.serial`が最小のものを置換する。同じserialならindexの小さいentryを選ぶ。
登録中、記録・pendingのleaseが残る資源、世代上限のentryは置換しない。候補がなければ
内部で待機せず容量不足を返す。失敗した準備を受理済み利用として新しく扱わない。

再利用候補はregistry容量分を準備したrecord配列、候補bitmapとdescriptor別cursorで保持する。
登録中やlease保持中のentryを候補に残して全資源を毎回走査しない。
nativeの外部参照はleaseを通し、解放通知を準備済みbitmapとindex queueでentryごとに集約する。
取得時には通知indexだけを消費して候補へ戻し、bitmap全体の反復走査をしない。
通知を消費した時点でも登録・世代・参照数を確認する。候補の選択だけでは旧entryを削除しない。
同descriptorの最小indexを使い、返却時はcursorを必要な位置へ戻す。候補の取得・返却で
整列Vecの後続要素を移動しない。descriptor表の直近lookupも再利用する。
descriptor表は空classも保持し、変更で新しいdescriptorが入るときだけ空classを再利用する。
class表とbitmapのmetadataは初期化で予約する。代償は常駐metadataの増加であり、
多数classが交錯する場合のbitmap比較と、明示作成上限時のoldest探索は線形のまま残る。
木nodeと回転規則を増やす案より小さな所有状態を選び、CPU比較の入力・結果と限界は
[現在の検証](../verification/current.md#runtime-review-improvements)に記録する。GPU速度の改善は主張しない。

再利用は旧内容の初期化状態とCPU provenanceを消す一方、実際のlayout・access・writer scopeは保持する。
`delete`もkeyを直ちに失効させ、その内容の初期化/provenanceを解放する。既に記録した命令が必要な
native参照・staging・planは別leaseでGPU完了まで保持し、削除済みproofだけがGraph slotを留め続ける進行性の循環を避ける。
新しいnative資源とViewの作成・命名がすべて成功してから置換し、失敗時には既存entryのallocation・
世代・予定状態を維持する。OOM時に暗黙のevictionや再試行を加えない。

これは空きentryを直ちに異なる作成条件で置換する方式に比べ、交互の寸法でnative作成を繰り返す問題を
避けるための保持方針である。代わりにidle資源の保持量が増え、上限時の置換中には旧・新資源が一時的に
共存する。`resource_capacity`は保持するentryの件数上限であり、byte budgetや作成成功の保証ではない。
resizeやlevel切替後には登録を`delete`してから`trim_idle_resources()`を明示呼出しできる。
これは未登録かつleaseなしの物理資源だけを解放し、登録やpendingを解除せず、pollやwaitも行わない。
pending・記録済み参照の退役後に再呼出しすると、新たにidleとなった資源を解放する。
返値は解放したnative資源数で、tombstoneに世代を残し、後の作成で古いkeyを復活させない。
世代上限のslotは解放できるが再利用しない。native作成失敗では空きslot・世代を変更しない。

代案の自動byte budget／resize専用evictionより、明示的なworking set境界を選ぶ。
通常の交互寸法再利用を保ち、隠れた待機や生存snapshotの無効化を避けるためである。
解放の時機はアプリが選ぶ必要があり、trim自体はregistryを一度走査する。
VMAは空blockを保持し得るので、解放数や論理byte減少をprocess VRAM減少と同一視しない。
`memory_stats()`のcache論理byteとVMA Allocation／blockの実統計を区別する。作成回数とCPU/GPU時間・メモリの比較は
[現在の検証状況](../verification/current.md)に記録し、件数削減だけからGPU高速化を主張しない。
上位のmesh常駐には別に件数・論理byte予算を設ける。常駐解除後のidle物理cacheやpending参照は
その常駐予算とは寿命が異なるため、この値をDeviceの総VRAM上限と説明しない。

`ExecutionPool`は初期化で指定した全枠のCommandPool・一次CommandBuffer・Fenceと返却配列を準備する。
予約を先に取得でき、空き不足は待たずに返す。待機・再試行は呼び出し元が選ぶ。
未投入キャンセルまたは実Fence完了後にpoolとFenceをresetし、成功した枠だけを再利用する。
reset失敗やDevice停止・lossでは再貸出しない。通常予約で失った枠を暗黙に再作成しない。
貸出中のleaseがpool状態を保持し、idle資源からpoolへの循環参照は作らない。

`TransferPool`はUpload／Readback各1本の固定容量bufferを共有する。
実buffer/allocationの所有はVMAへ任せ、内部領域は準備済みfree-range配列で配置・分割・結合する。
`RuntimeConfig::staging_ranges`は同時range lease数の上限（既定256）で、byte容量と独立に不足を返す。
全Bistroの間接描画では、材質グループごとのテンプレート転送が同時実行フレーム数だけ重なる。
サンプルは通常の転送枠にその件数を加え、共有byte容量には初期アップロード、
同時実行フレームの整列済みアップロード、readbackの最大値を使う。
頂点とインデックスをまとめた初期転送の末尾も、Vulkanの上限である256 byteのcache atomに合わせる。
[Vulkanの必須上限](https://docs.vulkan.org/refpages/latest/refpages/source/Required_Limits.html)
N個のlive rangeが作る空隙は最大N+1個であり、返却前に隣接空隙を結合して一時的な配列増設も避ける。
従来のVMA Virtual allocatorはmetadata nodeの成長を持つため、通常frameのhost確保ゼロ目標に対し、
準備済み配列で上限を保証できる方式へ変更した。代案はVMA側のhost metadata poolを独立に準備・検証する方式。
自前の範囲管理に伴う整列/結合の不具合と線形探索の費用がリスクであり、
alignment・byte/range容量・分割/結合とGPU/CPU leaseの回帰で正しさを確認し、速度優位は主張しない。
バッチ専用bufferより余剰容量を共有しやすくする一方、GPU完了とCPU利用終了を区別して回収する。

容量はCPU-addressableな非ゼロ値で、`max(4, nonCoherentAtomSize)`の倍数を要求する。
領域の位置と予約長も同じ境界へ合わせ、隣のleaseとcache atomを共有しない。
要求byte数と整列した予約byte数を区別し、空き不足で内部待機や無制限な増設を行わない。
この容量はbufferの要求byte容量であり、VMAの物理blockやDevice全体のVRAMの上限ではない。
virtual領域の空き不足とnativeメモリ確保のOOMも区別する。

Upload leaseは正常完了時に返す。Readback leaseはCPUコピー成功または結果破棄まで保持し、
読み取り失敗では再試行できる。共有Readbackの領域APIはCPU read用とする。
native実行枠は未読Readbackより先に再利用できる。
work配列は実GPU完了後に返し、CPU結果は独立したresult slotでreadback範囲を保持する。
`readback_len`でサイズを調べ、`readback_into(pass, &mut [u8])`で呼び出し側の領域へコピーする。
長さ不一致は検査を維持した固定サイズのエラーで返し、readback leaseを失わず再試行できる。
exportのCloneは準備済みslotの参照を増やすだけで、最後の外部owner返却時に範囲tokenを解放する。
pool所有者は通常処理中保持し、pool先終了後の最後のbacking解放は終了境界として扱う。
再取得した領域の過去のbyte列を、新しい要求の初期化済み内容として扱わない。
完了不明の処理はleaseを含めて保持し、領域だけを先に再利用しない。

### 大きなmeshの準備

常駐入力の利用側Graphは`VulkanEngine::begin_graph`から始め、必要なsnapshotだけを`connect_snapshot`する。
既存の準備済みframe scratchがmesh handleとrevisionごとの登録pinを保持し、Graphと一緒にcompile・cancel・submitへ進む。
接続はRasterや出力を追加せず、出力の形式・寸法・個数をViewへ固定しない。
常駐準備の`prepare_snapshot(snapshot, non_mesh_resources)`は利用側が指定した非meshの登録余裕を使い、
標準color/depth用の二枠を一律には要求しない。これは物理予約や全Graphの容量保証ではない。
submit成功時は通常のRuntimeのWork/resource leaseへ所有を渡してscratchを返す。
接続後のmesh入力はwriteと直接exportをGraph構築で拒否し、不変常駐版の保証を維持する。
標準meshのpublic Rasterも生成時に有限値を保証した`RasterParameters`からowned定数へ変換し、
Engine専用の描画能力に依存しない。genericな独自shader定数の意味と値域はtrusted資産の契約で定める。
一般import/履歴の拡張とは分け、既存のFence・同期・失敗時保持・物理資源再利用契約を変更しない。

入場したupload集合はcursorと未投入byte数を持ち、chunkごとに完了prefixと全meshを再走査しない。
進捗はsubmit受理後だけ更新し、cancel/unload/再入場で残量と退役索引を更新する。
通常のpollでは実際のretiring登録だけを調べ、予算内の常駐済み準備でLRU候補をsortしない。

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

設定・outcomeと利用手順は[二API共通の表示契約](rendergraph.md#windows-presentation)に従う。
`PresentationConfig`／`PresentOutcome`はVulkan内部の型ではなく、`crate::presentation`に置く。
backend選択のためにGraph、camera、入力処理を分岐させない。

native `presentation/`はGraph型・Snapshot・PhysicalLeaseを参照しない。
`swapchain`は能力選択と世代RAII、`slots`は再利用条件・present退役・acquire guard/取消を所有する。
`program`/`commands`はdescriptorとnative fullscreen記録を担当する。
Graph側の`work` adapterがsource leaseとcanonical scopeを渡し、submit受理時の予定状態と
完了時のserialだけを公開する。source所有はnative recordへ移さず、adapterが実完了まで保持する。
slotとguardの状態はprivateに保ち、状態遷移を所有者の操作へまとめる。
記録/CPUアクセスのprivate unsafe境界は同一Device、初期化、排他的使用と実完了の責務を明記する。

公開`prepare`/`present`がそれぞれpollを行ってから再作成へ進み、内部で同じpollを繰り返さない。
表示例は次回再描画期限を早期eventで後ろへ動かさず、ゼロ寸法ではevent/明示復帰期限を待つ。
復帰時の描画可否はWindow寸法から決め、Swapchain作成済みかどうかには依存させない。

`for_window`は`Arc<winit::window::Window>`を受け取り、Runtimeと`Presenter`を返す。
Surface所有者がWindowとInstanceを保持し、Deviceと同じInstanceに由来することを保証する。
対象をWin32に限定し、winitの`HWND`／`HINSTANCE`からashのWin32 Surface作成を直接呼ぶ。
別Window libraryや別OSのadapterを含む`ash-window`依存は持たず、D3D12だけの表示構成へashを追加しない。
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
表示元のcanonical stage/accessが正確に`FRAGMENT_SHADER`/`SHADER_SAMPLED_READ`で、
layoutが`SHADER_READ_ONLY_OPTIMAL`なら、先行する同queueの依存が後続の同じreadも覆うため、
source画像barrierだけを省く。Graph本体と同じexact read scope判定を使い、
初回表示、異なるscope、write、layout変更の依存とSwapchain側のbarrierは維持する。
writer自体は保持し、writeで変わるrevisionとcanonical状態から後続の必要依存を再計画する。
記録payloadのpreviousは必要なscalar scopeだけとし、取得したSnapshotは複製せずnextへ移す。
内容revision、初期化範囲、provenanceとwriterを保持し、submit受理時だけ予定状態を公開する。
source lease、未投入取消、GPU完了とpresent完了による退役、不確定失敗時の所有保持は変えない。
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
希望画像数は二API共通の2〜16とし、実Surfaceのmin／max制限へ合わせた実画像数を取得する。
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
`poll`・ready状態の`prepare`・`wait_idle`は完了した枠を返すが、旧Swapchainの最後のanchorを破棄しない。
旧世代は`max_retired_swapchains`内で保持し、明示した`collect_retired_swapchains()`が
GPU/present参照のない世代だけを待機なしで破棄する。dirty状態の再作成境界でも容量確認前に同じ回収を行い、
旧世代がpendingなら上限不足を返す。リサイズが終わった後の通常pollへnative破棄・Vec解放を流さないための方針である。
待機完了は実破棄の指示とは分け、必要な保守時機と旧世代の上限分の保持量を呼び出し側が選ぶ。
正常終了はGraph frameを回収し、Presenterを明示closeしてからnative所有者を破棄する。
Dropの保守的回収でも完了を証明できない場合は、Window・Surface・同期・Device等の所有連鎖を保持する。

## 範囲計画と性能評価

転送の参照計画・比較用`ByteRanges`は半開区間を`BTreeMap`で保持し、隣接・重複を統合する。
初期化追跡とcopyの競合判定で共用し、保持区間数Kに対する包含・重複検索をO(log K)、
N件の挿入列を償却O(N log N)で扱う。独自の木構造や空きメモリ配置は実装しない。
通常Graphの論理解析は準備済みAVL node arena、nativeの予定状態・CPU provenanceは
準備済み区間配列を使い、通常frameで木nodeや配列を確保/解放しない。
木の更新と配列の挿入/削除の費用は残るため、確保ゼロからCPU時間の改善を推定しない。

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

### 外部CPUトレースとの対応

CPUの実行箇所と待機の調査には、保存した最適化済みEXEとmatching PDBを使う任意のWPR収集を設ける。
手順・artifact identity・CLIは[ツールガイド](../guides/tooling.md#cpu-profiling)、
可変16-byte copy chainの同条件負荷は[開発手順](../guides/development.md#cpu-profile-workload)に従う。
この経路はRendererの公開APIや通常submitへETW依存や毎frameの計測を追加しない。

`profile_workload`の任意handshakeでは準備/warmup後に外部collectorが所有WPR sessionを開始し、
二つのWPR markerで計測処理の外側を囲む。markerでworking setをflushしない。
対象のQPC窓とmarker区間は同一ではなく、GO/DONEの配送やJSON処理・待機を含む差を記録する。
初期化を含める収集、測定loopだけの窓、native teardown後の正しさ確認を区別する。

SampledProfileのWeightは実行中CPU時間の標本推定で、呼び出し件数や関数一回の正確な時間ではない。
CSwitch/ReadyThreadから得るRunning、実行可能なReady、イベント待ちのWaitingは別々に集計し、
callerのbusy/yield/sleepによるpoll方針を測定条件に含める。
これはqueueのGPU timestamp、GPU完了までのlatency、表示FPSを代替しない。
実sampleのIP、対象PID/生存期間とImage_Loadの実baseからRVAを求め、
matching EXE/PDBによるinline/source帰属を確認する。未知symbolや端点、lost eventsをゼロに置き換えない。
既知RVAのoffline解決やCSV fixture試験と、実ETLのstack/source・欠落の受入は別の証拠である。
確認済み範囲と未実行項目は[現在の検証状況](../verification/current.md)へ記録する。

`tracy` featureを明示した場合は、これと独立に非管理者の手動CPU zoneを採取できる。
Graph begin/compile/解析、Vulkanのprepare・pass/draw準備・descriptor具体化、
record・pass/barrier・draw batch等のhost範囲へprivateなzoneを置く。
Graph構築のpass内overlapとexact-read圧縮、Vulkanのprepare prelude内のimport revision照合・
resource lease取得と、その後のcanonical snapshot copyにも独立zoneを置く。
これらは既存のnative CPU prepare計測範囲を変更せず、Graph/APIの保証や寿命処理も省略しない。
既定offでzone専用コード・metadata・依存を除外し、通常submitのnative呼出し・同期・寿命・検証は変えない。
`Client::running()`のguardはcollectorの接続を意味せず、on-demand切断時のclient費用も別に確認する。

Bistroはwarm-upと回収後に接続し、明示した測定zoneの完了後にENDを受けて画像captureと通常終了へ進む。
native clientの終了処理が送信queueを排出し、collectorの自然終了・保存を待つ。helperは通常完了でDisconnect停止を要求しない。
traceに含まれるROI後のreadback/teardownは解析で除外し、frame markをzone送信完了のbarrierへ流用しない。
Tracy zoneのinclusive/selfはhostの経過時間であり、OS charged CPU、WPRのsampling weight、
GraphのGPU timestampや表示latencyとは別である。長いnative wrapper zoneは呼出しを囲む費用で、
driver内の関数帰属を確認したことにはならない。CPU/QPC/TracyとGPUの絶対時刻対応も新たに導入しない。
固定client/toolと収集・費用比較は[Tracy手順](../guides/tooling.md#tracy-profiling)を参照する。

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

Graph結果の`SubmissionMetrics`／`GpuTiming`は`crate::profiling`の二API共通型を使う。
表示側は独立した`Presenter::set_profiling(bool)`で有効にし、`profiling_enabled()`で要求を確認する。
有効化はtimestamp能力と固定容量を確認する準備境界であり、Graph計測を暗黙に有効にしない。
`take_profile()`は完了済みの`PresentationMetrics`をFIFOで一件ずつ返す。
pendingと未消費sampleの合計は`max_frames`以内とし、満杯ならacquire前に容量不足を返す。
利用側はpollとともにsampleを消費し、暗黙に計測結果を捨てたり領域を増やしたりしない。

表示sampleはsource準備、acquire、record、submit、present API、退役のCPU区間と、
表示用fullscreen命令だけのGPU timestamp区間を持つ。
表示submitが受理されれば、その後の表示要求が拒否されてもsampleを回収する。
したがってsample件数は`Presented`件数や画面へ到達したframe数の保証ではない。
acquire／presentのCPU時間にはnative呼出しのpacing費用を含め、GPU区間へ加算しない。
退役時間は成功した回収のquery取得・所有返却を含み、失敗したpollや明示waitのblock時間は含めない。

profilingを一度も有効にしなければ、専用query poolを作らず、通常frameに各phaseのtimerを加えない。
無効化は将来の記録だけへ適用し、既に受理したsampleの回収・所有は維持する。
準備済みquery領域は無効化のたびに破棄せず、明示変更／終了まで保持する。
query取得のために通常frameで待機せず、Graph計測と同じ実Fence・availability・有効bit幅の条件を使う。
これは計測の費用を有効時へ分ける設計であり、profiling有効時の無費用や性能優位を主張しない。
VulkanはTOP_OF_PIPE／BOTTOM_OF_PIPE、D3D12はDIRECT queueのBOP timestampで命令区間を囲む。
native markerの意味と同じworkload／設定を併記し、短い区間の差をshader専有時間や普遍的なAPI速度差へ読み替えない。

`memory_stats()`は要求時にVMA統計を取得する。登録済み、pending等で保持中、再利用可能なcacheの
論理byte数、stagingの要求byte・整列予約byte・high-water、VMA Allocationとblockの件数・byte数を分ける。
画像の論理byte数はpacked texel量であり、optimal imageのAllocation sizeでもprocess VRAM使用量でもない。
このVMA統計はdriverやSwapchainの確保を含まない。
対応Deviceでは`VK_EXT_memory_budget`を有効にし、同じ照会でheap別のcapacity、
VMA Allocation/block統計とnative process usage/budgetを記録する。memory typeが共有するheapを重複集計しない。
usageはdriverの推定、budgetは変動する目安であり、物理pageの常駐や次の確保成功を保証しない。
非対応ではusage/budgetを`None`とし、heap容量やVMAのfallback推定で補完しない。
`vkGetPhysicalDeviceMemoryProperties2`への直接照会とし、VMAの`EXT_MEMORY_BUDGET` flagは使わない。
統計のためにallocatorのbudget方針や通常frameの更新処理を変更しない選択である。
`bufferImageGranularity`と`nonCoherentAtomSize`も記録するが、両者を同じalignmentとして扱わない。
[Bistroの常駐・更新受入](implementation-policy.md#bistro-residency)で実測結果を扱う。
製品・性能用のLayer設定は`new_with_validation`で明示でき、既定値は`development-validation`の有無で選ぶ。
要求したmodeとLayerから実際に観測した設定、未確認の情報を区別して記録する。

## 参照する仕様

- [キューとホスト同期](https://docs.vulkan.org/guide/latest/queues.html)
- [投入・状態・失敗時の保証](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html)
- [Lost Deviceの扱い](https://docs.vulkan.org/spec/latest/chapters/devsandqueues.html#devsandqueues-lost-device)
- [VMAのメモリmapping](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/memory_mapping.html)
- [VMA Virtual allocator](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/virtual_allocator.html)
- [Command資源の再利用](https://docs.vulkan.org/samples/latest/samples/performance/command_buffer_usage/README.html)

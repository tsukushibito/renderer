# RenderGraphの契約

Graphの宣言・解析はGPUとウィンドウに依存せず、通常のCPU構成で利用する。
Vulkan実行は`vulkan` featureで追加し、公開APIへash・VMAの型を出さない。
Windowsの`d3d12` featureは同じ`CompiledGraph`のbuffer upload・copy・readbackを実行する。
Graphの宣言・解析と、各backendが実行できる操作の範囲は区別する。
全体方針は[実装方針](implementation-policy.md)、native資源と完了管理は
[Vulkanの設計](vulkan.md)と[D3D12の設計](implementation-policy.md#d3d12-runtime)、
実行済みの確認は[検証状況](../verification/current.md)を参照する。

検証処理の配置・製品構成からの除去には[製品版と開発用の検証方針](../guides/validation-policy.md)を適用する。
以下は現在のAPI・動作の契約である。backend選択、追加検証、診断保持は独立に選ぶ。

次の開発は[実装方針のBistro計画](implementation-policy.md#bistro-roadmap)に従う。
B0で実シーンの要求・公開API利用例と[基盤の採用・移行範囲](implementation-policy.md#foundation-selection)を設計した。
B1では構築時owned命令とbinding/attachment由来アクセスを一般化し、nativeの低レベル所有を再利用して二APIで実証する。
現行の固定操作、内部表現、構築順による意味論、runtimeの再利用は維持義務ではない。
以下の現行契約は、変更を実装・検証するまで有効であり、計画だけで対応済みとは扱わない。

## ソースの責務と依存方向

`graph.rs`は公開型のfacade、Graph所有状態、identityとcompile入口を持つ。
子moduleの`resource`は論理資源・import・範囲、`pass`は宣言とowned命令、
`error`はprivate contextを持つ構造化エラー、`builder`は検査後のtransactionalな追加を担当する。
`accesses`/`compile`は共通解析を担当し、既存builder回帰群は`graph/tests.rs`へまとめる。
公開型はfacadeから直接reexportし、並行する互換APIを作らない。

Graphから`engine`の型を参照しない。`MeshData::upload_graph_slice`は不変なstreamと
既存のindex要約を選び、Graphの狭いprivate builderがowner/全stream長/alignment/範囲を検査して追加する。
engine側からGraphのpassを直接変更せず、任意のpublic byte uploadからmesh provenanceを作らない。
元のArc、slice位置とappend失敗時の状態を維持し、chunkごとのpayload複製や追加scanを入れない。

## 対応範囲と処理の境界

Vulkanの経路は、単一Graphics＋Computeキュー、物理aliasingなし、
2D画像のmip・layer・sample各1を対象とする。
bufferは半開byte範囲を指定し、未対応の画像subresource指定は拒否する。
論理ハンドルと物理資源の識別、内容の有効性と画像layout、
予定されたGPU状態と実際の完了を別々に管理する。

通常のパスは、管理下のcopy・Compute・Attachment clear・indexed mesh・最終合成という限定した操作を宣言する。
任意の生CommandBuffer、外部SPIR-V、ネイティブ外部ハンドルは受け入れない。
これにより、実行する操作と宣言した用途・範囲・bindingを対応づける。
汎用RHIや未信頼shaderのサンドボックスを提供する契約ではない。
依存するCompute処理は別ノードとし、パス間の同期を実行基盤が記録する。

| 実行基盤 | 現在の範囲 |
|---|---|
| Vulkan | buffer／画像copy、固定Compute、Attachment clear、indexed mesh、合成、import／export、上限付き再利用 |
| D3D12 | transient bufferのupload・copy・readback。単一DIRECT queue、committed resource、物理aliasingなし |

D3D12のprepareはexportとliveなimport・画像・未対応操作を明示エラーにする。
共有Graphを構築できることだけで、すべてのbackendがその操作を実行できるとは扱わない。

B1の最初の範囲として、Graph外の`program::ManagedProgram`と両runtimeの
Program、Uniform/Storage buffer、RGBA8/RGBA16F画像/View、nearest/clamp samplerの共通binding作成APIを用意する。
bindingはmanifest順に
実Programと全資源の版/範囲を保持する。Storage本文はGPU可変で、作成を初期化とは扱わない。
[管理下資源の契約](vulkan.md#managed-uniform)を参照する。
この作成APIとprivate GPU受入を、公開Graphの一般draw/dispatch対応とは扱わない。
owned pass・attachment/subresource由来の計画は`renderer-7ft.2.2`へ残す。

## producer・範囲・Attachment

資源は可変で、readは宣言順で直前までに書かれた内容を使う。
bufferの異なるbyte範囲には異なるproducerがあり得る。
一つのパスのreadをすべて検査してからwriteを反映するため、
`read_write`宣言自体では未初期化内容を初期化できない。
forwardの明示依存は実行順序を制約するが、readが読むversionを変更しない。
明示依存の先は意味的な前提として残す。liveな循環は常に拒否し、
`development-validation`ではdeadな部分の循環も診断する。製品構成は除去されるだけの循環を拒否しない。
ハンドルはappend-onlyなGraph内だけで有効で、別Graphでの利用を拒否する。

`Graph::image_desc(ImageHandle)`はownerと画像の型を検査し、`ImageDesc`を返す。
一時画像とimport画像、colorとdepthで同じ問い合わせを使える。
内容の初期化やGPU完了を要求せず、descriptorの取得がそれらを保証するわけでもない。
custom処理は入力handleから形式・寸法を取得して出力を作れるため、Viewの寸法を別に渡す必要はない。

構築・compileの失敗は`GraphError::kind()`で分類する。`GraphErrorKind`は非網羅の列挙型で、
入力不正、無効handle、owner不一致、未対応、import snapshot不一致、未初期化read、循環、
利用側のcustom失敗を区別する。`pass`／`resource`、名前、`AccessRange`を取得でき、
bufferは問題のbyte範囲、画像はmip・layer・aspectを返す。内部の画像用論理範囲`0..1`は公開しない。
循環は`cycle_passes()`で、先頭を末尾に再掲した有向経路として実際の参加passを返す。
同名passもhandleで区別でき、循環から出るだけの後続passは含めない。
構築中に拒否したpassはまだhandleを持たず、要求名だけをcontextへ残す。
別Graphのhandleから現在のGraphの同じindexを参照して名前を取り出すことはしない。

利用側のclosureは`GraphError::custom(message)`で独自の失敗を返せる。
エラーは名前とcontextを所有するが、GraphやGPU資源を保持しない。読みやすい`Display`と
`message`は残し、通常の失敗処理に文言解析を要求しない。
構造化contextの追加により、旧`GraphError { message }`という直接構築はconstructorへ移行する。
これは今回の公開API変更であり、既存literalとの互換層は追加しない。

宣言の範囲・用途・初期化は不要パス除去の前に検査する。
deadなreadも未初期化ならエラーになり、不要な処理で入力ミスを隠さない。
解析専用の`declare_pass`と実際の操作builderを分け、実行時にliveな解析専用パスを拒否する。
native記録中の任意の実行callbackは提供しないので、Graph構築や除去をCPUのゲーム更新の代わりにしない。

upload本文は不変の`Arc<[u8]>`で共有し、compileで全量を複製しない。
既存の`upload_buffer`／`upload_image`はVec入力を受け取り、共有表現へ一度変換する。
`upload_buffer_shared`／`upload_image_shared`へ同じArcを渡せば、Graphの再構築でも入力を共有できる。
compiled planは除去したパスの実行本文を保持しない。確定した順序に不要な明示依存列も保持しない。
`graph-diagnostics`だけがdump用の依存・寿命の解析情報とculled accessを保持し、
`CompiledGraph::dump()`を提供する。文字列は要求時に一度生成する。
`culled_pass_names()`も`graph-diagnostics`構成で提供し、保持済みのculled操作を参照する。
offではculled pass名を複製せず、live pass名は運用エラーの文脈と実行順の問い合わせに残す。
使われないtransient資源名もoffでは複製しない。live／exportと、投入時の再照合があるimport名は維持する。
この区分はcompile中の既存live access走査で求め、一時索引をplanに保持しない。
論理dumpにはアクセス用途も含む。`graph-diagnostics`構成のVulkanの`submit_with_diagnostics`は、成功した投入と
実際のloweringの診断文字列を返す。通常の`submit`はこの文字列を生成しない。
診断にはpass／論理resource、物理key・世代・native handle、import初期予定状態、
実記録barrierのstage／access／layoutと生成理由、最終予定状態、readbackのHOST_READ依存を含める。
文字列は資源の所有を保持せず、GPU完了tokenでもない。native handle等は実行ごとに変わるため、
安定した機械可読形式や実行間で同一のdumpは保証しない。

`Graph::raster(name, draw, attachments)`は名前付きの`RasterDraw`と`RasterAttachments`を受け取る。
drawはvertex／index buffer・index count・`RasterParameters`をまとめ、
attachmentsはcolorの`Attachment<[f32; 4]>`とdepthの`Attachment<f32>`を区別する。
各attachmentは画像、`LoadOp<T>`、`StoreOp`を持ち、`Clear(value)`だけがclear値を要求・検査する。
`Load`／`Discard`には未使用clear値を渡さず、native operandには固定値0を使う。
既定の描画引数には`RasterParameters::default()`を明示する。

Attachmentは画像全域をrender areaとする。`Load`は既存内容を要求し、
`Clear(value)`は全域を初期化する。loadの`Discard`は部分的なmesh描画だけで全域を
確立したとみなさない。storeの`Discard`は後続の内容を無効にする。
`Load`／`Clear`と`Store`の組だけが、meshの外側も含めた有効内容を引き継げる。
exportする資源の全域にも有効な内容を要求する。

メッシュは7個のf32（xyz、linear rgba）、stride 28 byte、u32の三角形indexを使う。
CPUのRust構造体を直接キャストせず、各値を明示的にbyteへ変換する。
`MeshData::new`はindex内容を一度検査し、最大値とcountを不変byte列に対応づける。
privateな管理下uploadはこの保証と既知のprefixを引き継ぐ。全量が確立した通常mesh描画は
index本文を走査せず、頂点数と実Device上限を要約から照合する。検査用のupload Arcも保持しない。
CPU assetや投入前のstaging入力自体の所有は別の必要な寿命である。

任意Graph入力ではruntimeがupload・copyから既知のindex内容を追跡し、index値が頂点数未満であることを確認する。
Deviceの実際の`maxDrawIndexedIndexValue`も確認し、確認できない内容や範囲外indexを
native命令の記録前に拒否する。featureを有効化していないことから上限を固定値へ置き換えない。

内容追跡はupload入力の共有sliceを参照し、GPU copyではbyte列をCPUで複製しない。
部分書き換え・copyの未知の穴を保持し、sliceを跨ぐindexも一時bufferを作らず検査する。
既知sliceは開始位置をkeyとする`BTreeMap`で管理し、write／copyでは対象区間と重なる部分だけを
検索・更新する。全segmentの移動や再sortを更新ごとに行わない。
一般入力のindex検査は同じ内容・index数について最大値を一度求める。
頂点数／Device上限が変わってもこの要約を比較し、同じindex本文を再走査しない。
write／copyによる置換で要約を失効し、snapshot cloneの後続更新も独立して扱う。
現在のindex用途とそのcopy元、import／exportしたbufferに必要な内容だけを追跡する。
import／exportしたbufferは後のGraphでindexに使えるため、元の入力を共有したまま保持する。
この保持は新しいCPUミラーの作成とは異なるが、小さなsliceが元の入力全体を保持する場合はある。

管理下要約はprivate markerに結び付け、一般writeでは失効する。位置を保つ完全copyだけが保証を継承する。
全countより短いdrawや部分copyを要約だけから保証済みとみなさず、証明できなければ`UnknownContents`として拒否する。
任意の部分byte更新・copy・将来のindex importに必要なprovenanceは、safe入力の保証として製品にも残る。
全Graph uploadのCPU保持を削除した実装とは説明しない。
[保証の移動](../guides/validation-policy.md#guarantees)と[Graphの分類](../guides/validation-policy.md#graph)を参照する。

## 論理的な依存と実行順

compile解析用の名前、初期化範囲と明示依存は構築Graphから借用する。
完成した`CompiledGraph`は必要な命令・資源・名前を独立所有し、呼出元の借用を残さない。
通常構成では解析済み実行順をmoveし、optional診断が同じ情報を保持するときだけ複製する。

内容のproducerを特定するdataflowと、同じ物理資源のRAW・WAR・WAWによる順序制約は区別する。
不要パス除去はexport・readback・外部に観測される書き込み・明示的な副作用から
必要なproducerを辿る。上書き済みwriterや不要なreaderを、順序制約だけで残さない。
nativeのstage・access・layoutは、残った実際の操作から具体化する。
トポロジカル順序だけではVulkanのメモリ依存は成立しない。

順序依存は資源の区間ごとのwriterと未処理readerから作り、全先行パスとの総当たりを行わない。
reader履歴はindexで参照する不変のarenaへ保持し、区間の分割では履歴を共有する。
writeで複数区間のreaderを辿る際も共有tailを重複して走査しない。arenaはcompileの解析中だけ存在し、
長い連鎖の破棄で再帰する所有構造にはしない。複雑な重複範囲で触る区間数や必要な依存自体が
増える費用は残るため、すべての入力に線形時間を保証するものではない。
開発構成では全宣言の循環を確認し、通常の実行順は残ったパスだけで順序依存を組み直す。
除去された途中のwriter／readerが、必要なパス同士の順序を消さないためである。

解析専用の`declare_pass`もsafe入力の保証境界であり、owner・範囲・用途を先に検査してから競合を拒否する。
共有の競合検査は宣言順で最初の衝突pairを選び、read/readや無関係な資源の総当たりをしない。
Aアクセスに対してO(A log A)の処理とO(A)の一時情報で、全readならO(A)、一時allocationなしとなる。
private builderが保証した入力のcompile再点検は`development-validation`へ分離する。
producer・初期化・culling・live hazard／topological計画・最終内容の確定は製品でも実行する。

## GPU状態と所有

この節のcanonical state、import／export、cacheとframe APIは現在のVulkan実行の契約である。
D3D12の限定したbuffer実行の所有と完了は[次節](#d3d12-buffer-lifetime)に記す。

同じruntime資源のimportは、物理資源と状態の同一性を維持する。
exportハンドルの取得はCPUで内容を読めることを意味しない。
単一キューへproducer Graph Aとconsumer Graph Bを投入する場合、
Bは受理済みのAの最終状態を使い、両者の間に必要な依存を記録する。
CPU readbackと再利用には実際の正常完了が必要である。
完了観測は単調なserialだけを保持し、完了した時点の入力byte列を含むsnapshotは複製・保存しない。
最新の予定状態は将来のindex検査に必要な内容を保持するが、全面上書きで不要になった旧入力を
完了情報のために保持し続けない。実際のGPU完了と資源の回収は従来どおりFenceと所有leaseで判定する。

各資源は直近のread範囲だけでなく、後続用途へ可視化する必要のあるwriterのscopeも引き継ぐ。
copyの書き込みを一度copyで読んだだけでは、vertex／index入力へも可視になったとはしない。
bufferのnative依存は現段階では全体を対象に保守的に記録し、
Graphのbyte範囲によるproducer解析と、native barrierの粒度を区別する。

`VulkanRuntime::submit`は実行枠を予約してから、一つのGraphを記録・投入する。
`Submission::export_buffer`／`export_image`は受理済み状態のsnapshotを返す。
これを次のGraphでimportして投入でき、AをCPUで待つ必要はない。
snapshotにはowner・世代・内容revisionがあり、別runtime、削除済み、内容更新前のsnapshotを拒否する。
読み取り専用の利用や画像layoutの変更だけでは内容revisionを進めない。
複数のViewやGraphを先に構築しても、内容が変わっていなければ順次投入できる。
nativeのstage/access/layoutと最後の利用serialは投入時のcanonicalな予定状態から計画し、
古い同期状態を再利用しない。書き込み・clear・discard後は旧内容のsnapshotを拒否する。
更新後は`refresh_buffer`／`refresh_image`で取得し直し、Graphを再構築・compileする。
実行時にもcanonicalな状態と照合するため、compile後の変更を取り逃さない。
owner・世代・内容revisionの照合は製品でも行う。同じ世代のdescriptorは不変で、初期化状態の
変更はrevisionを進めるため、descriptor/初期範囲の全量再比較は`development-validation`だけで行う。

exportした資源はruntimeへ登録され、明示的な`delete`で登録を解除する。
CPUハンドルをdropするだけでは登録を解除しない。
未登録資源は保持参照がなくなってから再利用でき、同じ作成条件ならallocationとViewを保持する。
同じ作成条件がなければresource容量までentryを追加し、異なる条件のcacheも保持する。
上限時だけ、空いている未登録entryのうち最後に受理した利用serialが最も古いものを置き換える。
同じserialならentryのindex順とし、登録中・保持参照があるentry・世代上限のentryを選ばない。
置換先の作成・命名が成功するまで旧entryを維持する。再利用時はnative同期状態を保持し、
以前の内容の初期化情報とCPU provenanceを新しい論理資源へ引き継がない。
resource容量は保持する物理entry数の上限であり、DeviceのVRAM budgetではない。
異なる作成条件を保持する分だけidleメモリが増え、置換時は旧資源と新資源が一時的に共存する。
保持量は論理byteと実際のVMA allocation／blockの統計を分けて観測する。
exportを保持し続けた場合も、容量不足を返し、内部で待機・無制限増設はしない。

`Submission::wait`のtimeoutは`None`で、同じsubmissionを再度待てる。
正常完了後は`SubmissionResult::readback`へreadbackパスのハンドルを渡す。
CPUコピー成功時にそのstaging領域を解放し、二重消費と別Graphのハンドルを拒否する。
読み取り前の結果を保持していても、完了した実行枠は先に再利用できる。

継続するフレームには`submit_frame`を使える。Runtimeが上限付きで未完了処理を所有し、
返す`FrameSubmission`は`FrameId`とexport snapshotだけを持つ。
このメタデータをdropしても待機・キャンセル・回収は行わない。
`poll_frame`はtimeout 0で実Fenceを観測し、正常完了した一件を`FrameCompletion`として返す。
`wait_frame(timeout)`は結果を保持したまま一件の完了を明示的に待つ。
先に完了済みの結果を調べ、必要なときだけ最古の未完了Fenceを残り予算で待つ。
timeoutや完了を証明できない失敗ではpendingの所有を維持する。
完了したnative枠、upload、descriptor、記録用参照は回収し、未読readbackだけを結果へ残す。
後続の観測で失敗しても、先に返した結果を失わないよう一件ずつ返す。

`pending_frames`は観測前の処理数であり、既にFenceがsignalされていても回収前は数える。
上限は`execution_slots`で、満杯なら投入前に容量不足を返す。
自動waitや無制限な増設でframe数を隠さない。実行枠は従来の`submit`とも共有するため、
frame数が上限未満でも他の利用によって実行枠が不足する場合がある。
終了用の`drain_frames(timeout)`は予算内で明示待機し、完了結果を破棄する。
CPUで必要な結果は先に`poll_frame`で受け取る。予算切れと完了不明の失敗では所有を維持する。
通常のフレーム進行を、従来の`Submission`のDrop待機へ落とさないための追加経路である。

記録から完了まで、使用するbuffer・image・View・pipeline・descriptor・stagingを保持する。
論理的な削除だけでGPU使用中の資源を破棄しない。
記録用の操作はstagingとnative命令に必要な引数を持ち、upload本文を持たない。
正常完了後の結果にはreadback用の保持を残し、記録用の操作は解放する。
投入前のキャンセルや未投入を証明できる失敗と、完了不明の処理を区別する。
timeoutは内容を有効にせず、未完了の所有を維持する。
Device loss後は有効な出力を返さず、新しい仕事を止める。

受理済み状態は投入成功後、ログ出力より先に公開する。
submissionの番号は投入前に消費し、後続のログがpanicしてもrevisionを再利用しない。
結果を返せなかった新規export登録はguardで解除し、実行中の所有はpending側に残す。
native submitのOOMは未投入として扱えるが、それ以外の不確実な失敗はDeviceを停止する。
後始末のFence待機が後に成功しても、未確定の内容を以前の状態として再利用しない。

通常の失敗は`RuntimeErrorKind`で入力・未対応・Renderer容量不足・古い資源・owner不一致・
Device loss／利用停止・native失敗に分類する。
`std::error::Error::source`から元の操作・Device・native結果を調べられる。
native OOMと、設定済みpoolの空き不足は別の分類とする。

<a id="d3d12-buffer-lifetime"></a>

## D3D12のbuffer状態と所有

`D3D12Runtime::prepare`は、共有`CompiledGraph`のculling後の順序を使ってnative命令を準備する。
producerや範囲の解析をD3D12用に作り直さず、Graph構築で確立した不変の保証を引き継ぐ。
native固有の資源条件・上限と、未対応操作の拒否は準備境界で行う。
不要パスにしか現れない画像・操作は実行しないため、それらだけを理由に拒否しない。

DEFAULT bufferの`COMMON`から、legacy barrierでcopy source／destinationへ状態を追跡する。
Graphはbyte範囲を解析し、native状態遷移は全bufferを対象にする。
同一buffer内の非重複copyは独立したscratch bufferを介し、宣言したbyteの結果を保つ。

`D3D12PreparedGraph`は未投入のcommand・資源を保持し、Dropでキャンセルできる。
`submit`／`submit_prepared`は非同期で、`D3D12Submission`がGPU参照を所有する。
`poll`または`wait(timeout)`が正常完了を返した後だけ、結果の`readback(PassHandle)`でCPUへコピーする。
timeout後も保持して再待機でき、別Graphのhandleとreadbackの二重消費は拒否する。
runtimeを先に終了しても、prepared／submission／未読結果が必要なnative所有者を保持する。

Device removalや投入後のsignal失敗を有効な内容として返さない。
完了不明の処理は停止と所有保持へ進め、未投入の作成・記録失敗と区別する。
Drop時の対象Fence待機と保持の規則は[D3D12 runtimeの設計](implementation-policy.md#d3d12-runtime)を参照する。
この段階にはcanonical import／export、永続key、資源poolや上限付きpending frame管理を追加していない。

## 同期と一般推奨の適用

[ベストプラクティス](../guides/vulkan-best-practices.md#synchronization)に従い、
Copy、Compute storage、fragment sampling、vertex/index入力、color/depth attachment、
readbackのHOST_READを実際の用途へ対応づける。
Dynamic Renderingのパス間barrierはrendering scopeの外へ置く。
readbackがない処理にはHOST_READ依存を追加しない。

単一キュー、保守的な同期、通常のdescriptor set、固定shaderは正しさを確認する基準である。
vendor依存の高速化や高度なパス統合は採用しておらず、性能改善を主張しない。
利用側独自shader/pipelineとパスAPIはB0/B1で優先して設計・実証する。
複数キューと物理aliasingはBistro実測から採否を決める後続候補であり、同じ優先度へまとめない。

## 履歴と段階的な拡張

履歴は永続keyからfreshなsnapshotを取得し、前のGraphの予定状態を次のGraphへimportする。
前フレームのCPU完了待機は不要だが、内容を保持する履歴を単なる一時画像として貸し出さない。
読み取りだけなら内容revisionは変わらず、保存済みsnapshotからの複数consumerを構築できる。
内容を書き換える履歴chainでは、受理された更新後にfreshなsnapshotを取得する。
初回と明示reset、extent変更では内容を初期化し直す。
旧履歴の登録は最後に使用するGraphの投入後に解除でき、pending側の所有が完了まで維持する。
二枚交互という番号だけでは安全性を証明せず、予定アクセスと最後の利用を追跡する。

M3のエンジン入力は、scene/ECSの意味、Graph構築、native記録、GPU寿命を分けて接続する。
custom passの挿入はGraph構築時に行い、標準描画と同じproducer解析・内容検査・同期を通す。
現時点の実行操作は管理下のshaderに限定するが、依存解析に固有のshader実装を持ち込まない。
任意shaderを導入するときはshader/pipelineの所有、binding metadata、宣言との照合、
ABIと信頼境界の検証が追加で必要になる。現在の限定操作だけでそれらまで保証したとは扱わない。
公開パスの形はB0で複数の利用コードから設計し、B1でVulkan/D3D12の実行結果を確認する。
独自の効果を追加するためにGraph本体やeffect列挙型を変更しないことを受入条件とする。
構築時に所有された命令を蓄える方式、実行callback方式などは比較して選び、記録時期や型名を先に固定しない。

代表利用コードによる[公開API比較と第一案](implementation-policy.md#public-api-comparison)では、
型付きbinding／attachmentからアクセスを導き、構築時に命令を所有する案をB1の出発点とした。
frame内借用が可能な遅延callbackとも同じ利用例で比較する。CPU試作は将来の独自program、
depth-only／color-only、HDR、MRT／mip等の契約を検討するもので、上記の現行対応範囲を拡張しない。
基盤は[B0の比較](implementation-policy.md#foundation-selection)でnative上位置換＋低レベル再利用を選んだ。
実装変更までは現行のproducer・import・寿命契約を維持する。

## エンジン入力と標準描画

GPU非依存の`engine::Engine`は容量付きのmesh/material登録を持つ。
型付きハンドルはowner・index・generationを検査し、更新は同じハンドルの内容を進め、
削除と再登録では世代を分ける。meshは不変の入力byte列、materialはlinear RGBA tintに限定する。
scene、ECS、asset loader、カメラの意味は呼び出し側が所有する。
RendererはModel構造を持たず、mesh/materialと描画入力を扱う。
Modelを含めない責務境界と理由は[実装方針](implementation-policy.md#21-ゲームエンジンへ提供するもの)を参照する。

`View`、`Draw`から作る`FrameSnapshot`はmeshの版とtint・行列を所有し、
後続のCPU更新・削除や一時借用に依存しない。`Matrix4`はcolumn-major、column vectorとし、
`clip_from_world * world_from_model`を合成する。clip depthは0〜W、viewport高さは正、通常Zである。
有限なdrawパラメーターはsnapshotでprivateな検証済み値として確立し、batch構築で再走査しない。
public `Graph::raster`のraw入力検査は維持する。batchのowned operandはfallible iteratorから直接作り、
途中失敗時はpassを追加しない。
投影とhandednessは入力行列で選ぶ。Rustのstruct layoutをGPU ABIとせず、
頂点は明示byte化し、描画引数も64-byte MVPと16-byte tintへ明示的に変換する。

snapshotの`build_graph`はGPU・windowなしで標準Graphを構築できる。
この単独経路はmeshをGraph内でuploadするため、小さい入力やCPU解析用とする。
持続的な実行では、利用側の`Engine`がCPU assetとsnapshotを所有し、`VulkanEngine`は
runtimeとGPU常駐を所有する。同じsnapshotを独立したrendererへ渡すことができ、
バックエンド別のCPU asset登録は要求しない。

GPU常駐はmeshのowner・index・generation・revisionで識別し、`MeshCacheConfig`で件数と
論理byte数を制限する。CPU assetの削除と別に`unload_mesh`で常駐を解除でき、
新しいworking setの準備では、必要でなくpinもない常駐meshを決定的な順に退避する。
必要working set自体が予算を超える場合と、一時的なpending資源の容量不足を区別する。
通常Viewのcolor/depth二資源を含む物理容量も入場判定に使い、論理mesh予算内でも
古い登録が描画先の枠を埋めるときは退避する。prepared pinで必要枠を確保できなければ明示エラーを返す。
準備済みframeは必要な登録をpinし、submit後のnative所有は通常のGPU leaseへ引き継ぐ。
CPU更新・削除後の旧snapshotも、そのimmutableな版として独立して準備できる。

`prepare_snapshot`と`poll_uploads`は明示的に分割uploadを進める。
一つのmeshや全入力がstaging容量を超えても、同じ上限付きpoolを再利用する。
`Ready`は必要なbyte列がすべてqueueへ受理されていることを意味し、GPU完了ではない。
内容の版ごとに元のimmutableな入力を共有し、chunkごとのCPUコピーを永続的な鏡像として保持しない。
同じqueueの後続描画はCPU完了待ちを要求しない。通常の準備・poll・build・submitに暗黙waitを入れず、
明示的な待機には`wait_uploads(timeout)`を使う。uploadの回収が別の描画frameの結果を消費しないよう、
所有するsubmissionを分ける。`build_snapshot`は準備済み常駐を使い、uploadや待機を隠して実行しない。
Fence完了後の結果・計測取得だけが失敗した場合も、退役済みuploadを回収して元のエラーを返す。
完了不明のFenceエラーやtimeoutは同じ回収扱いにせず、所有を維持する。
標準の不透明描画はowned draw listを一つの`RasterBatch`操作にする。
空draw listはclear-only、非空ならcolor/depthを一回clearし、宣言された順に全drawを描いてStoreする。
Rasterノード数はdraw数によらず一件で、custom Compute・明示的なcomposite・readback等は別ノードのままである。
同じmeshの再利用と、異なるmeshの組合せの両方を扱い、drawの行列・tint・index数を所有する。
compileでもdraw配列を`Arc`で共有し、GPU完了まで必要な資源・操作の所有を維持する。

外部bufferアクセスは資源ごとに範囲を集約し、同じbufferがVertexとIndexの両方なら両用途を残す。
indexは各drawの頂点範囲とDevice上限について記録前に検査し、後のdrawが不正でも部分的にsubmitしない。
canonical importは`ResourceKey`をkeyとするprivateな索引で検索し、同一性とsnapshot整合性の検査を保つ。
Vulkanの用途集約も資源keyの順序付きmapを使い、多数の異なるmeshを一つのパスへ集めても全入力対を走査しない。

`Graph::raster`等の単一draw操作は参照経路と明示的なGraph構築に残す。
今回のbatchは固定shaderの標準opaque描画をまとめる契約で、公開PassEncoderや任意パスの自動統合ではない。
別の描画先、Compute、copy等の必要な境界と同期は維持する。
共有mesh／異なるmeshの画像・同期・性能の確認は[検証記録](../verification/current.md#draw-listの本実装と比較計測)を参照する。

拡張点はopaque描画後の完全なlinear RGBA8画像である。
Viewと同じextentで初期化済みの画像を返し、通常のGraph builderで処理を追加する。
closureはGraph構築時に実行され、GPU記録中にsceneへ再入しない。
返却画像のhandle・形式・寸法・初期化はGraphの通常の検査を通す。
不要なcustom branchも通常どおり除去される。
返された完成画像を直接exportし、無変換の最終compositeと別画像を毎Viewに強制しない。
`FrameOutputOwnership::Frame`はこのGraphで作った出力、`Caller`は利用側がimportした出力を表す。
`release_output`はframe所有の元のkeyだけを登録解除し、caller所有の登録には触れない。
CPU経路の`FrameGraph`、持続的な実行経路の`PreparedEngineFrame`から通常のGraphを取り出し、
build、compile、submitを分けられる。利用側の履歴処理や測定を標準描画と同じGraphへ接続できる。
準備済みframeとplanのGraph identity、元の出力と必要exportを投入前に照合する。
buildはGPU実行枠の予約ではない。内容が変わる別submitや外部keyの削除ではimportが失効するが、
共有meshの読み取りだけを行う別Viewのsubmitでは失効しない。
`RenderedFrame`のframe所有出力は明示解放まで登録されるため、表示や履歴等のconsumerを投入後に
`release_output`を呼ぶ。CPUメタデータのDropに暗黙の登録解除を結び付けない。

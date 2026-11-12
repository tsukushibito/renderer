# RenderGraphの契約

Graphの宣言・解析はGPUとウィンドウに依存せず、通常のCPU構成で利用する。
Vulkan実行は`vulkan` featureで追加し、公開APIへash・VMAの型を出さない。
全体方針は[実装方針](implementation-policy.md)、native資源と完了管理は
[Vulkanの設計](vulkan.md)、実行済みの確認は[検証状況](../verification/current.md)を参照する。

## 対応範囲と処理の境界

最初の経路は、単一Graphics＋Computeキュー、物理aliasingなし、
2D画像のmip・layer・sample各1を対象とする。
bufferは半開byte範囲を指定し、未対応の画像subresource指定は拒否する。
論理ハンドルと物理資源の識別、内容の有効性と画像layout、
予定されたGPU状態と実際の完了を別々に管理する。

通常のパスは、管理下のcopy・Compute・Attachment clear・indexed mesh・最終合成という限定した操作を宣言する。
任意の生CommandBuffer、外部SPIR-V、ネイティブ外部ハンドルは受け入れない。
これにより、実行する操作と宣言した用途・範囲・bindingを対応づける。
汎用RHIや未信頼shaderのサンドボックスを提供する契約ではない。
依存するCompute処理は別ノードとし、パス間の同期を実行基盤が記録する。

## producer・範囲・Attachment

資源は可変で、readは宣言順で直前までに書かれた内容を使う。
bufferの異なるbyte範囲には異なるproducerがあり得る。
一つのパスのreadをすべて検査してからwriteを反映するため、
`read_write`宣言自体では未初期化内容を初期化できない。
forwardの明示依存は実行順序を制約するが、readが読むversionを変更しない。
明示依存の先は意味的な前提として残し、循環はdeadな部分も含めて拒否する。
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
compiled planには除去したパスの名前・アクセス等の診断情報を残し、実行本文は保持しない。
dumpは要求時に一度生成する。
論理dumpにはアクセス用途も含む。Vulkanの`submit_with_diagnostics`は、成功した投入と
実際のloweringの診断文字列を返す。通常の`submit`はこの文字列を生成しない。
診断にはpass／論理resource、物理key・世代・native handle、import初期予定状態、
実記録barrierのstage／access／layoutと生成理由、最終予定状態、readbackのHOST_READ依存を含める。
文字列は資源の所有を保持せず、GPU完了tokenでもない。native handle等は実行ごとに変わるため、
安定した機械可読形式や実行間で同一のdumpは保証しない。

Attachmentは画像全域をrender areaとする。`Load`は既存内容を要求し、
`Clear`は全域を初期化する。loadの`Discard`は部分的なmesh描画だけで全域を
確立したとみなさない。storeの`Discard`は後続の内容を無効にする。
`Load`／`Clear`と`Store`の組だけが、meshの外側も含めた有効内容を引き継げる。
exportする資源の全域にも有効な内容を要求する。

メッシュは7個のf32（xyz、linear rgba）、stride 28 byte、u32の三角形indexを使う。
CPUのRust構造体を直接キャストせず、各値を明示的にbyteへ変換する。
runtimeはupload・copyから既知のindex内容を追跡し、index値が頂点数未満であることを確認する。
Deviceの実際の`maxDrawIndexedIndexValue`も確認し、確認できない内容や範囲外indexを
native命令の記録前に拒否する。featureを有効化していないことから上限を固定値へ置き換えない。

内容追跡はupload入力の共有sliceを参照し、GPU copyではbyte列をCPUで複製しない。
部分書き換え・copyの未知の穴を保持し、sliceを跨ぐindexも一時bufferを作らず検査する。
既知sliceは開始位置をkeyとする`BTreeMap`で管理し、write／copyでは対象区間と重なる部分だけを
検索・更新する。全segmentの移動や再sortを更新ごとに行わない。
index検査は同じ内容・index数・頂点数・Device上限について直近1件の結果を再利用する。
write／copyによる置換は成功・失敗のどちらのcacheも失効させ、条件が変われば再検査する。
不変内容のsnapshot cloneはcacheを引き継げるが、元とcloneの後続更新は独立して失効する。
現在のindex用途とそのcopy元、import／exportしたbufferに必要な内容だけを追跡する。
import／exportしたbufferは後のGraphでindexに使えるため、元の入力を共有したまま保持する。
この保持は新しいCPUミラーの作成とは異なるが、小さなsliceが元の入力全体を保持する場合はある。

## 論理的な依存と実行順

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
全宣言の循環を確認してから、残ったパスだけで順序依存を組み直す。
除去された途中のwriter／readerが、必要なパス同士の順序を消さないためである。

## GPU状態と所有

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
snapshotにはowner・世代・revisionがあり、別runtime、削除済み、更新前のsnapshotを拒否する。
更新後は`refresh_buffer`／`refresh_image`で取得し直し、Graphを再構築・compileする。
実行時にもcanonicalな状態と照合するため、compile後の変更を取り逃さない。

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

## 同期と一般推奨の適用

[ベストプラクティス](../guides/vulkan-best-practices.md#synchronization)に従い、
Copy、Compute storage、fragment sampling、vertex/index入力、color/depth attachment、
readbackのHOST_READを実際の用途へ対応づける。
Dynamic Renderingのパス間barrierはrendering scopeの外へ置く。
readbackがない処理にはHOST_READ依存を追加しない。

単一キュー、保守的な同期、通常のdescriptor set、固定shaderは正しさを確認する基準である。
vendor依存の高速化や高度なパス統合は採用しておらず、性能改善を主張しない。
複数キュー、物理aliasing、一般PassEncoderと任意shaderは後続の対象とする。

## 履歴と段階的な拡張

履歴は永続keyからfreshなsnapshotを取得し、前のGraphの予定状態を次のGraphへimportする。
前フレームのCPU完了待機は不要だが、内容を保持する履歴を単なる一時画像として貸し出さない。
read-onlyの利用でもrevisionが進む現在の契約を守り、保存済みsnapshotを無条件に繰り返し使わない。
初回と明示reset、extent変更では内容を初期化し直す。
旧履歴の登録は最後に使用するGraphの投入後に解除でき、pending側の所有が完了まで維持する。
二枚交互という番号だけでは安全性を証明せず、予定アクセスと最後の利用を追跡する。

M3のエンジン入力は、scene/ECSの意味、Graph構築、native記録、GPU寿命を分けて接続する。
custom passの挿入はGraph構築時に行い、標準描画と同じproducer解析・内容検査・同期を通す。
現時点の実行操作は管理下のshaderに限定するが、依存解析に固有のshader実装を持ち込まない。
任意shaderを導入するときはshader/pipelineの所有、binding metadata、宣言との照合、
ABIと信頼境界の検証が追加で必要になる。現在の限定操作だけでそれらまで保証したとは扱わない。
一般PassEncoderの公開形は第二APIで実証するM4で検討し、未使用の汎用RHIを先に固定しない。

## エンジン入力と標準描画

GPU非依存の`engine::Engine`は容量付きのmesh/material登録を持つ。
型付きハンドルはowner・index・generationを検査し、更新は同じハンドルの内容を進め、
削除と再登録では世代を分ける。meshは不変の入力byte列、materialはlinear RGBA tintに限定する。
scene、ECS、asset loader、カメラの意味は呼び出し側が所有する。

`View`、`Draw`から作る`FrameSnapshot`はmeshの版とtint・行列を所有し、
後続のCPU更新・削除や一時借用に依存しない。`Matrix4`はcolumn-major、column vectorとし、
`clip_from_world * world_from_model`を合成する。clip depthは0〜W、viewport高さは正、通常Zである。
投影とhandednessは入力行列で選ぶ。Rustのstruct layoutをGPU ABIとせず、
頂点は明示byte化し、描画引数も64-byte MVPと16-byte tintへ明示的に変換する。

snapshotの`build_graph`はGPU・windowなしで標準Graphを構築できる。
持続的な実行には`VulkanEngine`がmeshのGPU bufferを保持し、同じmeshを毎frame uploadしない。
cacheは永続keyで識別し、使用時にfresh importを取得する。
更新・削除後も先行frameの所有を維持し、CPUに保存した旧snapshotも独立して扱う。
標準経路はclear-onlyのノードで空draw listも初期化し、各drawが同じcolor/depthをLoadして描く。

拡張点はopaque描画後・最終composite前の完全なlinear RGBA8画像である。
Viewと同じextentで初期化済みの画像を返し、通常のGraph builderで処理を追加する。
closureはGraph構築時に実行され、GPU記録中にsceneへ再入しない。
返却画像のhandle・形式・寸法・初期化はGraphの通常の検査を通す。
不要なcustom branchも通常どおり除去される。
CPU経路の`FrameGraph`、持続的な実行経路の`PreparedEngineFrame`から通常のGraphを取り出し、
build、compile、submitを分けられる。利用側の履歴処理や測定を標準描画と同じGraphへ接続できる。
準備済みframeとplanのGraph identity、元の出力と必要exportを投入前に照合する。
buildはGPU資源の予約ではなく、その後の別submitやCPU更新でimportが古くなった場合は再buildする。
`RenderedFrame`の出力も明示deleteまで登録されるため、表示や履歴等のconsumerを投入後、
不要な出力keyの登録を解除する。CPUメタデータのDropに暗黙の登録解除を結び付けない。

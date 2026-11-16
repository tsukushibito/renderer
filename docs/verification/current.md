# 現行実装の検証状況

最終確認日: 2026-11-17。以下の比較元と実行範囲は各記録に従う。実行手順は[開発ガイド](../guides/development.md)、
現在の契約は[RenderGraph](../design/rendergraph.md)と[Vulkan設計](../design/vulkan.md)、
全体範囲は[実装方針](../design/implementation-policy.md)を参照する。
過去の検証表・比較はGit履歴に残し、この文書は現在の確認範囲を示す。

<a id="bistro-pbr"></a>

## Bistroの実行用cookと二APIのForward PBR

`renderer-7ft.3.1`／`.3.2`と、実装後レビュー・計測の`renderer-bj6`。
利用側の`tools/bistro/cook.py`と`bistro` exampleを公開Graphへ接続した。
Model／scene／材質recipeをRenderer本体へ追加せず、同じcook・camera・shader契約をVulkanとD3D12で実行する。
実行方法は[開発ガイド](../guides/development.md#bistro-pbr)、
現在の境界は[実装方針](../design/implementation-policy.md#bistro-runtime-cook)を参照する。

### 素材・取込の保証

固定Bistro入力の全1,296 mesh、1,591 primitive、132材質、405画像、2,832,120三角形を保持した。
実行用は48 byteのworld-space position／normal／tangent／UVと、primitiveごとのu32 indexである。
MikkTSpaceのper-corner結果を再indexし、非一様変換・反射のnormal／tangent／sign／windingを処理する。
2,900,027頂点、139,201,296 byteの頂点と33,985,440 byteのindexになった。
元の19退化面・27,956退化UV面も削除していない。
有限のゼロ接線486 cornerには理由を記録したfallbackを使い、うち474件は実素材の投影角度重みの消失である。
非有限値やMikk生成失敗をfallback成功に置き換えない。

全405 DDSの供給済み3,529 mipを再サンプルせず復号し、baseは固定PNGと全画像で完全一致した。
raw mip count 0のnormal 20件は全て1×1であり、欠落mipの補完ではない。
色はnative sRGB sampling、normal／ORMはlinear dataとして扱い、BC5の正Z復元とgreen反転は全供給mipへ適用する。
画像のpacked合計は5,011,178,404 byteで、native確保量やVRAM常駐量とは区別する。
alpha分類120 opaque／7 mask／5 blend、glass 12段とlabel 5段の全ゼロalphaを保持した。

独立した全量cook 2回は125.047秒／138.162秒で、manifestと全payload hashが完全一致した。
manifest SHA-256は`d74e9863328c238ea4b2a8fc5b617fe28879acf904a46d6b59b4b6874488d4f5`。
独立レビューでも全mesh／node／primitive／材質／画像／sampler対応に欠落・重複を認めなかった。
Rust側も取込時に範囲・形式・index最大値・basis・hashを確立し、画像を一枚ずつ検査・uploadする。
`GeometryData`の不変要約をGPUへ引き継ぎ、通常drawのためのindex本文保持・再走査を避ける。
元のCPU geometryとupload ownerはGPU完了後、通常frame窓へ入る前に返す。

### PBRと実装後レビュー

B2はGGX metallic-roughness、normal、AO、emissive、両面、MASK／BLENDを実行する。
照明は固定の太陽と明示した定数diffuse ambient 0.03、合成はlinear RGBA16F、
SDRはclampと一度だけのsRGB OETFである。IBL・影・露出・PBR Neutral・Bloom・AAは後続B3／B4の範囲に残す。
nativeはsRGB・全2D mip chain upload・単一mip／全chain sampling・LinearRepeatを明示準備する。
D3D12の大小bufferと画像は全execution共通の物理class数を指定し、in-flight数で全assetを重複確保しない。
登録資源、記録・GPU・CPU consumerが終わる前に再利用・破棄しない。

レビューで、Vulkanの全mip refresh、D3D12の準備容量縮小、index全量更新の古い最大値、
頂点＋index同時upload容量、実ロードmanifestとidentityの対応、camera入力境界、
fixtureのview照合、Windows拡張pathの包含検査を修正した。
旧D3D12 callerのglobal画像数も、従来の同時実行予算を維持する個数へ移行した。
画像layout検査がbuffer self-copyを拒否する回帰は、既存全lifecycle試験で検出し画像だけへ限定した。

材質のGPU gateは集計depthだけで済ませず、独立したf64参照を持つ17 panelで
metallic・roughness・normal scale・接線回転／符号・UV・sRGB・emissive・正面／裏面・alphaを分けて照合する。
2×2 sRGBのUV probeは、定数UV・swap・V反転・filter後decodeを検出する。
alpha-zeroは同じAPIで独立に描いた空sceneのHDR／SDR／depthと全byteを一致確認する。
float16 clearの変換をCPUの最近接丸めへ固定できないため、対照自体も許される隣接half値・SDR・depthを検査する。
これによって通常材質の数値許容誤差を広げていない。

### 1080pの画像・確保・native受入

Windows 11 x64、Core i5-14600KF、RTX 3060／driver 610.88、Rust 1.99.0。
1920×1080、2 in-flight、変化するroute入力24 frame＋warmup 8と、固定7視点のHDR／SDR／D32Fを確認した。
strictはRenderer追加検証＋Vulkan Core／SyncVal、D3D12 Debug Layer／GBV、
shippingは追加検証・専用診断をコンパイル除外しnative要求をDisabledとした。
Vulkan shippingのcore／sync観測は`None`であり、falseへ補完していない。
両構成で各API独立2 process、計8 runが成功し、最終native所有者破棄後まで警告・エラーはゼロだった。
各runの通常窓はRust 4 event・VMA host 3 event・native作成／破棄8 eventが全て0、resetは別計上した。
setup／shutdownのpositive controlsも確認した。driver内部の未観測確保はRendererの保証へ加えない。

受入前に固定した画像profileは、fixture HDRの絶対＋相対誤差各1/1024・SDR 1 code・depth 2^-20、
scene HDRの絶対0.005＋相対0.01・SDR 3 code・depth 2e-6、超過画素は全画素の0.1%以下である。
全画素・全channelを集計し、輪郭等を除外しない。同API反復はbit一致とする。
7視点×各構成の同API反復／二API比較、計56照合が全て成功した。

実素材のalpha-zero 2材質・24 primitive・222三角形も、全sourceをロードしてから同じ選択で検査した。
strict／shipping各4 runで元の7視点がHDR clear／SDR 118／depth 1を保つことを確認し、
glass／label近景の指定領域ではopaque overrideによってHDR／SDR／depthの全てが変わった。
各構成16件のzero-coverage検査と11件の画像照合が成功した。opaque overrideはこの検査専用で、通常描画は元のalphaを保つ。

最初の1080p比較ではlabel近景のdepth 29,538画素が基準を超え、最大差は2.980232e-6だった。
native viewport／depth／copy／UBOの不一致はなく、clip行列積の積和順序に敏感な近景だった。
頂点のclip座標だけを明示した順序の`precise` FP32積・和へ固定し、生成SPIR-VのNoContractionとDXIL命令も検査した。
camera／ABI／cook／固定条件・比較閾値は維持したまま、近景pilotのdepth差はbit単位で0になり、全視点の再受入も通過した。
FMA機会を失う可能性は記録し、速度向上とは主張しない。

材質／UV／alphaとgeometry容量のGPU gateはstrict／shipping各3件、
既存Compute／custom Raster／汎用資源は各構成3件、WSIは各構成2件が成功した。
shippingの既存全lifecycle確保試験4件も成功した。
release workspace CPUは全feature391件／shipping375件、Bistro Python94件（Windows symlink権限のCPU試験1件skip）、
CPU調査tools 56件、Clippy・fmt・warnings拒否Rustdocを確認した。

### 通常releaseのPBR計測

同じ機材・cookを使い、`9950282`の通常releaseを一度だけbuildし、両APIで同じEXEを使った。
実効featureは`vulkan,d3d12`で、追加検証・Graph診断・memory countersはコンパイル除外し、native検証要求はDisabledとした。
EXE SHA-256は`8fd2ba02c2f1771a6e2837f2c4a7df75e7b7d13c16648f53814b4355ec0119ac`。
PE/PDBのGUID・age、埋込conditionsを含むsource hash、cook・shader identityを保存した。
1920×1080、全1,591 primitive、cullingなし、2 in-flight、固定60Hz入力経路の1,800 frame、warmup 3,600 frame（二周）。
API×内部CPUタイマー＋GPU timestampの無効／有効×独立3 process、計12 runを順番を入れ替えて直列実行した。
build・cook・他のGPU試験・offline画像解析は計測と並行させなかった。ETW収集・管理者実行は使用していない。

以下は各runのframe平均から求めた3 runの中央値、括弧はrun間の最小〜最大で、単位はms/frameである。

| API | 通常process CPU | 通常main thread CPU | 通常wall/frame | GPU平均（計測有効時） |
| --- | ---: | ---: | ---: | ---: |
| Vulkan | 19.583（18.212〜20.148） | 19.462（18.125〜20.061） | 20.081（19.059〜21.214） | 4.906（4.488〜5.024） |
| D3D12 | 32.674（31.241〜33.967） | 32.613（31.207〜33.915） | 33.785（33.014〜35.545） | 8.479（8.158〜8.958） |

process／main CPUはOSのcharged timeである。wall/frameはROI全体のQPC時間をframe数で割り、最後のdrainとresult returnを含む。
ロード・upload・prepare・warmup・最終capture・shutdownはROIへ含めない。
frameごとのbuild開始〜完了観測にはqueue滞在と後続host処理を含むため、上のwall/frameや表示遅延とは異なる。
headlessのyield pollでありVSync／present／表示FPSの測定ではない。GPU値はtimestampを有効にした別processの観測である。

| API | Graph build | compile | submit | poll／retire | result return |
| --- | ---: | ---: | ---: | ---: | ---: |
| Vulkan | 11.759 | 1.680 | 5.807 | 0.825 | 0.005 |
| D3D12 | 11.922 | 1.699 | 19.833 | 0.328 | 0.002 |

この表は通常runの呼出側QPC区間の各中央値で、CPU占有時間や排他的なnative関数費用ではない。
中央値を別々に求めるため合計はwall中央値と厳密一致しない。
このPBR条件ではD3D12のsubmit区間が長く、VulkanのCPU／GPU時間が短い。
driver内部の原因や全workloadでのAPI優劣をこの区間計測だけから断定しない。
両APIでGraph構築も大きいので、後続の最適化はこの負荷で原因を特定してから比較する。
D3D12の計測有効runではnative `record`が18.064〜19.018ms/frame、中央値18.068msだった。
これは命令記録、descriptor設定、barrier、export取得、query終了、Closeまでのhost経過区間である。
Vulkanはdescriptor準備の一部を`prepare`へ計上するため、同名phaseを同じ仕事内容として単純比較しない。
実スタックからの原因特定と最適化前後の比較は`renderer-4wl`へ記録した。

内部CPUタイマーとGPU timestampをまとめて有効にしたprocess CPU中央値はVulkan 20.139ms、D3D12 33.568msだった。
無効時に対する観測差は+2.84%／+2.74%、wallは+5.87%／+2.16%。
独立3 runのばらつきを含む記述値であり、負の差を高速化と解釈したり、単独のquery費用へ帰属したりしない。
無効時のnative phaseとGPU時間は欠測`null`とし、0で補完しない。
全12 runのframe数・実行identity・終了状態が一致し、ROI後に別途描くroute frame 0は、API内で計測有無をまたいでもHDR／SDR／depthがbit一致した。
全1,800 frameの画像を保存・比較したという意味ではない。
先行したwarmup 60 frameの独立12 runも補助記録として保持した。通常process CPU中央値は18.950／33.524msで、API差の傾向は同じだった。
主結果には固定の二周warmupを満たす表の値を使い、両batchを混ぜて集計しない。
独立レビューでも全reportのhash・数値・ROI・同一EXE・画像hashを再照合し、不整合を認めなかった。
ドキュメントは17 HTML page・2,236 local link/resource/fragment、32 diagram instanceを検査し、viewer・theme・mobile・printの表示を確認した。
これは新しいPBR負荷の基準測定であり、PBR未実装時との速度改善率ではない。

素材全量の更新・実VRAM予算の`.3.3`、B3以降の画質、他機材・長時間soak・device lossはこの受入へ追加していない。
詳細と実行identityは[ソースレビューの証拠](source-review.json)の`bistro_pbr`へ記録する。

<a id="cpu-stack-profiling"></a>

## 最適化済みCPUスタック・schedulerの調査環境

`renderer-jat`。容量エラー・資源探索・共通形式の改善を公開した`607bdf9`を基準に、
[CPU調査CLI](../guides/tooling.md#cpu-profiling)と、二API共通の`profile_workload`を追加した。
`profiling`はreleaseを継承し、full debug情報とstripなしだけを変更する。
最適化・assert・overflow・LTO・codegen設定を調査用に変えず、追加検証とnative metricsも独立させた。
保存したEXE/PDBをhash・GUID/ageで再確認し、source hash・toolchain・実効featureと結び付ける。
専用UUIDのWPR instanceだけを操作し、開始失敗や結果不明時には所有を推測してcancelしない。
UAC昇格やOS設定変更はCLIへ組み込まず、収集だけを管理者プロセスから実行する。
一度起動した管理者プロセスで複数runを収集でき、build・通常試験・保存済みETL解析は管理者権限を要しない。

Windows x64、Intel Core i5-14600KF、RTX 3060／driver 610.88、Rust 1.99.0。
WPR 10.0.26100.9818、WPA exporter 11.7.383.39833、xperf 10.0.26100.4654、
行解決にはLLVM 18.1.4を使用した。`CPU.Verbose`でSampledProfile・CSwitch・ReadyThreadのstackを収集した。
最初の通常権限での開始は`0xc5585011`で失敗し、OSのUAC確認後に実収集した。
raw ETL・system-wide CSV・symbolsはローカルに保持し、共有する証拠は対象processの集計に限定する。

### 実サンプルからソースまでの対応

Vulkan・256資源・8,192 frameの最終runでは、QPCの16組の実値が同じETL時刻原点に一致した。
測定窓は`0.3785166..3.4578062 s`、main TIDのrunningは3,071.8978ms、readyは7.2695ms、
未分類off-CPUは0.1223msだった。未分類をblockedやゼロへ置き換えていない。
3,061個のleaf sampleは全てCount=1で、weightは3,040.2765ms。sample数は関数の呼出回数ではない。
Events/Buffers lostはともに0。実ETLの専用BEGIN/END markerもQPC窓を囲むことを確認した。
全threadのrunning/ready/blocked合計はwallを超え得るため、main threadと分ける。
schedulerのrunningはCPUへ割り当てられた区間であり、割込み等も関わるため、
OSのcharged CPU時間やsample weightと同値とはしない。

Images表の実ロードアドレス`0x7FF668840000`は、PEのpreferred base `0x140000000`と異なる。
採取した命令アドレスから実baseを引いたRVAを対応PDBで解決し、12アドレス全てでソース行を得た。
Renderer由来の16スタックフレームでは、manifestの正規化source hashと行の存在も全て一致した。
例えば実IP `0x7FF66889E779`は`GraphWork::submitted`の`work/record.rs:280`、
`0x7FF66887C04B`は`RecycleQueue::drain_into`の`graph/cache.rs:39`へ対応する。
関数entryの代理アドレスを採取IPとして使っていない。caller stack・inline・未解決native frameも保持する。
D3D12の256資源runでも、26,461 sampleから実IP 12個を解決し、全てでソース行を確認した。
repository由来の20 inline frameもhashと行が一致し、`FrameSubmission::wait`、
`Registry::bind`／`commit`、`bounded::prepare`、Graph構築・解析・submitへ対応した。
同runの`n/a` stack 204件はCPU/selfの費用を保持してinclusiveのframeへ分割せず、欠損として記録した。
最適化による行位置の曖昧さ、driverやOSの未解決symbol、短い処理のsampling漏れは残る。

実CSVの確認で、WPAの既定groupingがprocess小計だけを出すことが分かり、leaf出力へ修正した。
また同一Old/New PID・TIDのself-switchでは過去のLastOut/Readyが残る実例があり、
このidentityを明示確認した行だけoff-CPUを0として扱う。近接時刻から推定しない。
Ready/Standby/DeferredReady、Waiting、未分類状態を分け、staleな起床時刻による二重計上を拒否する。
readying側stackとnew thread stackは列の意味を保持し、後者を待機原因と断定しない。

### ビルドと収集の費用

cleanな同一sourceのrelease／profiling archiveを使い、全条件を同じ管理者contextとした。
16 byteの可変入力を4／256資源のcopy chainへ流し、1 in-flight・yield poll・seed 42、
500ms warmup後に65,536／8,192 frameを実行した。
API×規模×通常release／非収集profiling／収集中profiling×独立3 processの計36回が成功し、
同じframe数のcontent digestと最終native validation結果も一致した。
順序を入れ替え、測定中にbuild・GPU試験・offline ETL解析を並行させなかった。
12本のヘッドレスETLのうち11本はlossゼロで、各条件に2〜3本、3,061〜26,461 sampleの有効なrunがある。
Vulkan・4資源の3回目は36,095 eventの欠落でWPA/xperfが拒否し、stack/scheduler受入から除外した。
保存停止前のWPR statusは0だったため、statusだけでlossゼロとはしない。
raw失敗記録を保持し、`-tle`／`-tti`で通したり欠落をゼロで埋めたりしなかった。
表のOS CPU counter・内容照合とETLの品質は独立した観測である。

以下はprocess累積CPU時間をframe数で割った値の、独立3 runの中央値である。
main thread CPU、QPC wall、各runのmin/maxと個別値も証拠へ残す。

| API／資源数 | 通常release | 非収集profiling | 収集中profiling | 収集中／非収集の中央値比 |
|---|---:|---:|---:|---:|
| Vulkan／4 | 51.737µs | 50.545µs | 52.929µs | 4.7% |
| Vulkan／256 | 373.840µs | 375.748µs | 379.562µs | 1.0% |
| D3D12／4 | 152.349µs | 140.429µs | 200.748µs | 43.0% |
| D3D12／256 | 957.489µs | 1,001.358µs | 1,668.930µs | 66.7% |

これらは人工的な1 in-flight転送負荷の値であり、API全般の優劣や表示FPSを示さない。
debug情報を加えた差にもrun変動があり、無費用とは主張しない。
特にD3D12は変動が大きい。4資源の非収集profilingは68.4〜170.5µs、収集中は78.4〜263.2µs、
256資源はそれぞれ988.0〜1,020.4µs／1,102.4〜3,229.1µsだった。
表の比は3 runの記述統計であり、一定の計測overheadや因果効果を保証しない。
収集中の遅さだけをRendererの不要処理と判定しない。
通常性能の比較は非収集runを使う。clock・affinity・powerと他のdesktop負荷は固定していない。
外部markerのCLI開始を含む経過時間中央値はbegin 77.6ms／end 76.8msで、QPC測定窓の外にある。
停止・ETL mergeのwall時間もRendererのCPU処理へ加算しない。
ライブラリへETW依存や毎draw/frameのmarkerは追加せず、専用文字列・collector処理はPython側だけにある。
caller exampleのphase timerと既存runtime/GPU計測、native検証、memory counterは区別した。

### 表示の処理とpacing、品質確認

修正後の`windowed`を両API・VSync ON/OFF・各独立2 process、24 warmup＋1,024 frameで収集した。
8本全てでlossゼロ、実process start/endとmain＋App loopのsample stackによるmain TIDを確認した。
ETLのROIはstartup/warmup/teardownを含む実process寿命であり、
非warmupのframe間隔・native phase統計とは別の母集団である。

| API／VSync | 定常frame間隔中央値の2 run範囲 | process全寿命のmain scheduled-running | 同blocked |
|---|---:|---:|---:|
| Vulkan／OFF | 1.548〜1.570ms | 0.933〜0.969s | 0.941〜0.984s |
| Vulkan／ON | 16.654〜16.703ms | 8.507〜9.181s | 7.782〜8.428s |
| D3D12／OFF | 1.524〜1.553ms | 1.103〜1.169s | 0.856〜0.899s |
| D3D12／ON | 16.659〜16.675ms | 7.914〜9.006s | 7.911〜8.922s |

ONでは9,203〜10,245回のbackpressure retry、OFFでは0回を記録した。
configured 1ms retryと`winit::wait_for_messages_impl`を含む待機stackを対応づけ、
event-loop待機に属するblocked区間はOFFで0.700〜0.778s、ONで6.733〜7.989sだった。
ONでもscheduled-runningが残り、全経過時間をVSync待ちへ分類していない。
driver/OS内の未解決処理、収集の影響、native呼出しのelapsedも含むため、
retry件数やstackの長さだけから不要処理とは断定しない。
ready・未分類区間・process境界の未観測残差も保存し、全寿命の時間を1,024で割って定常CPU費用にしない。

最初の表示captureでは、完了後のwinit最終`AboutToWait`が解放済みbackendをpollしてpanicする不具合を検出した。
終了状態をshutdown開始時に確定し、終了後callbackへ再入しないよう修正した。
CPU回帰と、修正後の全8計測runでpanicなしを確認。既存の起動native crash／WSI stall課題とは別である。
製品／strictの表示smoke各APIは24 frame・6遷移・入力／全画素digest／geometryが一致し、最終診断もゼロだった。
ヘッドレスの両API・4／256資源・両構成で可変入力8 frameを確認し、
通常窓のRust 4event、VMA host 3event、native作成・破棄8eventが全0だった。resetは別計上する。
shippingの両WSI全lifecycle確保試験も再確認した。winit同期callerの確保とRendererの確保を分け、native/VMAは全窓を含める。

統合版はPython 55試験、release workspace製品337／全feature350試験、windowed CPU各12試験、
全target Clippy、fmt、warnings拒否Rustdocが成功した。
最初のCIはD3D12単独構成で、測定サンプルのVulkan専用importが未使用となりClippyが拒否した。
専用helperをVulkanの使用箇所で参照する形へ直し、単独APIの製品／開発構成も確認した。
計測済みEXE/PDB/sourceは保存したcommitへ結び付けたままで、import整理後の再計測として扱っていない。
mdBook 17ページ・2,170 local link/resource/fragmentと、browserで32個のdiagram／theme／mobile／printを確認した。
既存preprocessorのmdBook版警告は残るが、生成・リンク・描画チェックは通過した。
build manifest、各run、counterの全値、受理／拒否したtrace、実IP／ソース対応と確認範囲は
[ソースレビューの証拠](source-review.json)の`cpu_profiling_environment`へ記録する。
raw ETLとsystem-wide CSVは含めない。他機材／Linux GPU、長時間soak、OOM／Device loss、Bistro全体の速度は未確認である。

<a id="runtime-review-improvements"></a>

## 容量エラー・資源探索・共通画像形式の改善

`renderer-5xv`と子課題。比較元は`483e304`。
両Runtimeの`capacity()`が不足資源・回復操作・判明した上限と必要数を返す。
未準備や固定上限超過を通常backpressureと分け、同Graph内の累積staging／range／資源不足も
失敗時に判定する。追加の診断走査を通常成功経路へ持ち込まない。
D3D12表示例の3440×1440 verifyは19,814,400 byteを要求し、16 MiB上限を準備前に明示拒否する。
Graph／Programの画像形式は単一の`renderer::ImageFormat`とし、用途別native formatと能力検査は維持した。
READMEも二APIの現行対応範囲と三層の利用入口へ更新した。

Windows x64、Intel Core i5-14600KF、Rust 1.99.0、最適化有効。
ビルド完了後、他のビルド・GPU試験を並行させず、変更前後の順序を交互にした独立3 processを比較した。
各入力を最低100ms warm-upし、31 sampleを採取。以下は各run中央値3個の中央値である。
各sampleは反復batchの1cycle平均であり、個々のframeのlatencyやp95ではない。

| CPU fixture | 変更前 | 変更後 | 削減率 |
|---|---:|---:|---:|
| Vulkan 64候補・同class、全取得＋逆順返却 | 1.756µs | 0.562µs | 68.0% |
| Vulkan 64候補・8class | 1.961µs | 0.838µs | 57.3% |
| Vulkan 1024候補・同class | 88.063µs | 8.750µs | 90.1% |
| Vulkan 1024候補・16class | 191.119µs | 15.638µs | 91.8% |
| D3D12 登録1024＋一時8、取得とArc返却 | 1.850µs | 0.080µs | 95.7% |
| D3D12 一時512・同class | 56.384µs | 5.076µs | 91.0% |
| D3D12 登録1024＋一時512・16class | 190.423µs | 7.282µs | 96.2% |

両harnessの通常測定窓はalloc／alloc_zeroed／realloc／deallocが全0だった。
D3D12の小入力8資源は82.8→79.3nsと差が小さく、広い負荷での速度優位の根拠にはしない。
Vulkanの各候補が異なる64classは1.751→1.262µs、1024候補・512classは144.325→117.703µsで、
class交錯時の比較走査が残ることも確認した。初期のsmall-case回帰をlast-hit cacheで調整してから再計測した。
VulkanはGitの旧cacheを同一harnessで直接compileし、D3D12のreferenceは旧`entries.iter().position`の
条件を同一の所有取得・返却窓で実行する。D3D12のnative所有境界だけを`Arc<()>`へ置き換え、
Vulkanのdescriptor keyは整数で与える。native実行・frame全体・GPU速度の比較ではない。

代償は準備済みclass／node／bitmap metadataの常駐量増加である。
D3D12のlease保持候補の走査、Vulkanの多class比較と明示作成時のoldest走査は残る。
clock・affinity・power設定は固定していない。別CPU／GPU／driver、Bistro全体の効果は未確認。
全入力・各runのraw sample、p95、source hashと確認範囲は
[ソースレビューの証拠](source-review.json)の`implementation_review_followup`へ記録する。
再実行は[CPU資源探索の比較](../guides/development.md#resource-reuse-measurement)を参照する。

統合版ではfmt、CPUのみ／二API表示のshipping／全featureの全target Clippy `-D warnings`と
release通常試験167／333／346件、windowed CPU試験各11件、warnings拒否Rustdoc、READMEのCPU例が成功した。
RTX 3060／driver 610.88上のstrict実GPU21件、shipping実GPU11件とCPU確保試験1件も成功。
両API・両構成の表示smokeは各24frame・6遷移を受理し、入力・全画素digest・可視画素数が全て一致した。
strictはCore/SyncValまたはDebug Layer/GBVの実効と、最終owner解放後までの診断ゼロを確認した。
shippingは追加診断なし・native Requested Disabledであり、外部注入の不存在を証明するものではない。

shippingのCPU、両buffer、Vulkan Engine画像の全lifecycleに加え、両APIのWSI通常窓で
Rust 4event、VMA host 3event、Renderer発行native作成・破棄8eventのゼロを再確認した。
native resetは別計上。最初のEngine画像fixtureは以前から通常forward用`mesh()`の明示準備がなく失敗し、
setupの準備一覧へ追加して再実行した。baselineと統合前のfixture blobが同一であることも確認した。
これは試験準備の修正であり、Programを通常submitで暗黙作成する回避策は追加していない。
大画面verify拒否はCPU回帰で確認し、実desktopの3440×1440 resize操作は未実行である。
最初のCIでは表示なしVulkan構成で表示専用の予約エラーhelperが未使用となり、Clippyが拒否した。
helperへ呼出元と同じWindows／presentation条件を付け、表示なしVulkan単独／二APIの
製品・開発4構成でも全target Clippyを確認した。広域のlint抑止や実行処理の変更は加えていない。

## 二APIのWindows表示・自由カメラ・通常frame計測

`renderer-7ft.2.6`。共通の`PresentationConfig`／`PresentOutcome`とアプリ所有のevent loopを使い、
同じ公開標準forwardまたは独自GraphをVulkan WSI／D3D12 DXGIへ接続した。
`presentation`はAPI選択と独立し、D3D12単独の表示にもVulkanを要求しない。
契約と選択理由は[表示の独立性](../design/implementation-policy.md#122-presentationの独立性)、
再実行は[Windows表示](../guides/development.md#windows表示)を参照する。

Windows x64、Rust 1.99.0、RTX 3060／driver 610.88、同一LUID。
GPU作業は逐次実行し、時間計測の最終runはビルド・他の検証を並行させずに実行した。

| 確認 | 結果 |
|---|---|
| 同じ入力・二構成 | 標準forward／独自Raster＋標準outputを、両API・strict／shippingで各24 frame実行。全画素のdigest、既知palette、左上clear画素、可視geometryを照合し、8経路が完全一致 |
| 自由camera入力 | 共通controllerへ5段の移動・相対mouse・右ボタン解放・focus lossを再生し、変更したcameraで実GPU描画。両API・両構成で移動／視点変更のdigest変化、mouse解放後の移動継続、focus loss後の停止を確認 |
| Window寿命 | 両API・両構成の24 frame smokeで3回resize、VSync往復、最小化・復帰、最終present直後の終了を実行。native fixtureでは取得後cancel、再present、未回収のpresent所有、runtime先行破棄と最終closeを確認 |
| strict／shipping | release＋Core/SyncVal／Debug Layer＋GBVと、追加検証・専用診断をコンパイル除外したnative Disabledを独立processで実行。strictは最終native解放後もwarning／error／corruption／callback失敗ゼロ |
| 通常frameメモリ | 各API・各構成の独立WSI fixtureで18変動source・36 present。shippingはbuild／compileからGPU・present退役、readback、結果・slot返却までRust alloc／alloc_zeroed／realloc／dealloc、VMA host、native作成／破棄が全0。resetは別計上 |
| 時間計測 | 640×480・3 draw、API×VSync ON/OFF×各3 process。24 warm-up＋96計測frameを各runで実行。CPU build／compile／prepare／record／submit／poll／retire／結果返却、Graph GPU、表示用GPU、acquire／present API、retryとframe間隔を別記録。計測runにreadbackとmemory counterを含めない |
| 既存回帰・品質 | 独自Raster／一般資源履歴／混合Graphのstrict・shipping各3件、D3D12 query寿命のstrict・shipping各1件が成功。release CPU全feature334件、二API表示＋memory製品321件、CPUのみ166件、windowed例10件が成功。全target Clippy、warnings拒否Rustdoc、fmt、固定bindings／既存shader再生成照合も成功 |

D3D12は一つのflip-model Swapchainを維持し、当該PresenterのGPU使用とcommand参照が戻った後、
明示変更境界で`ResizeBuffers`を行う。Vulkanはmaintenance1 present fenceと通常GPU参照の両方を待つ。
両APIともゼロextentを保留し、通常の不足・未準備時は暗黙waitや増設を行わない。
表示拒否後も受理済みGPUのsource・命令・descriptorを保持し、別出力への切替を利用側へ制限しない。
内容revisionは読み取り表示で変更せず、実資源とsnapshotを投入境界で照合する。

計測は明示opt-inとし、query・結果領域を固定容量で準備する。
無効化は以後の投入だけに適用し、pending sampleは完了・消費まで保持する。
表示sampleはGPU投入ごとに一件で、表示要求が拒否された投入も含む。accepted frameとの一対一対応は主張しない。
外側CPU合計とnative phaseは重複するため加算しない。測定末尾も通常の非blocking poll・結果返却を完了してからcloseする。
明示capacity blocking waitは各runで0、event loopのretry間隔1msはframe間隔へ含まれる。

最終3 runの中央値は、VSync OFFの要求間隔がVulkan **1.531–1.543ms**、D3D12 **1.531–1.553ms**、
ONはそれぞれ **15.146–16.725ms**／**16.634–16.802ms**。
Graph GPU区間はOFFで **6.400–6.464µs**／**9.216µs**、ONで **7.360–7.424µs**／**9.216µs**。
小さい表示負荷と明示retryの基準値であり、最大FPS、shader占有時間、Bistro性能、APIの普遍的な優劣には読み替えない。
VulkanのTOP_OF_PIPE／BOTTOM_OF_PIPEとD3D12のBOPというnative timestampの違いも保持する。
raw sample・中央値・nearest-rank p95・source hashは[current-metrics.jsonの`windowed_presentation`](current-metrics.json)、
メモリの全epochと境界は[frame-memory.jsonの同名欄](frame-memory.json)に記録する。

strictではD3D12 InfoQueueのinfo文字列保持が通常epochへ非同期に到着する場合があり、
初回は16 alloc／1249 byteを観測した。診断baselineとして別記録し、製品のゼロ保証を診断費用で代替しない。
時間の初期試行では他の検証との並行実行によるばらつきもあったため、上表は逐次測定した最終3 runを用いた。
Window受入はwinitの一process一event loop制約に従い、APIごとの独立processで実行する。

一回のVulkan shipping WSI試験は3分以上停滞して停止した。段階ログを加えた直後の試験と、
最終の独立3 processは各約1秒で成功し、原因と停滞箇所は特定できていない。
追加した計測の有限性・lock範囲は監査したが、native／winit内部の停止かどうかも未確定である。
未再現の留保として`renderer-1ji`に記録し、既存のstartup crash課題と同一原因とは扱わない。

OSからwinitへの実キー／マウス配送は未検証で、Computer Useによる追加確認はツールのアプリ利用が未承認のため未実施。
present受理・maintenance fence・D3D12 queue markerを実画面の表示完了とせず、PresentMon等のscanout測定も行っていない。
他GPU／driver、Linux実GPU、実OOM／Device loss、一般拡縮・HDR表示、Bistro PBR／影／tone mapping／Bloomは未実行。
新しいvendor最適化、通常CIへの実GPU必須化や時間閾値は追加していない。

## 利用側Raster shaderとblend・depthの二API実行

`renderer-7ft.2.8`。staticなtrusted Raster descriptorから頂点属性・inline定数・
読み取りbinding・blend／depthを具体化し、標準Programと利用側Programを同じGraph経路へ接続した。
契約と選択理由は[RenderGraph](../design/rendergraph.md#program由来bindingとshader-stageのowned計画)、
再実行は[開発手順](../guides/development.md#owned-raster-acceptance)、source hashと生reportは
[メモリ検証データ](frame-memory.json)の`custom_raster`を参照する。

Windows x64、Rust 1.99.0、RTX 3060／driver 610.88。同じLUIDの両APIを逐次実行した。

| 確認 | 結果 |
|---|---|
| 利用側shaderと三構成経路 | 24-byte頂点、32-byte VS／FS定数、三groupの疎なUniform／StorageRead／画像／samplerを使用。標準forward直接、postの独自Raster交換、標準opaque＋独自半透明＋標準出力copyを同じ公開APIで実行 |
| 内容・順序・depth | 8ケース×2周期、4出力・588 byte／frame、計9408 byte／API／runを独立期待値とAPI間で完全一致。straight／premultiplied alpha・加算・置換、逆draw順、depthで遮蔽されるdrawとdepth非更新、0／1／複数drawを確認 |
| 変更・寿命・失敗 | 記録／pending／結果ハンドル保持中の再準備拒否、65個の異なる資産の容量拒否後も旧集合で描画、集合交換後の旧plan拒否（bindingなしも含む）と再準備による復帰、cancel／記録枠不足、pending中Uniform／画像の更新とdelete、公開plan／runtime破棄後の結果消費を確認 |
| strict／shipping | 追加検証＋Core/SyncVal／Debug Layer＋GBVと、追加検証・専用診断を除外したnative Disabledを別processで各3回成功。strictは最終native解放後もwarning／error／corruption／callback失敗ゼロ |
| 通常frameメモリ | 各API・各構成の独立processで24 warm-up＋24変動frame、3 pending＋3保持結果を測定。build／compile／prepare／record／submit／完了／readback／poll・退役／全slot返却まで、Rust alloc／alloc_zeroed／realloc／dealloc、VMA host alloc／realloc／free、native作成／破棄・descriptor確保解放は全0。resetはVulkan48／D3D12 96として別計上 |
| 既存実GPU回帰 | 一般資源／履歴と既存混合Rasterのstrict／shippingが各成功。D3D12 native9件、Vulkan Raster native10件も厳格構成で成功 |
| CPU・品質 | release CPU166件、全feature331件、二API製品＋memory-profiling310件、D3D12単独製品194件／追加検証＋graph診断195件が成功（Rustdocを含む）。全targetの全feature・製品・D3D12単独診断構成Clippy、warnings拒否Rustdoc、fmt、shader再生成・bindings再生成照合が成功。HTML17 page・2050 local linkとMermaid32図のviewer／theme／mobile／printを確認 |

保証済みのABI・頂点属性・shader footprintはtrusted取込で確立し、実Device能力はpipeline準備で確認する。
safe Graphでは定数byte数、binding／資源範囲、attachment／depth条件を空drawも含めて保証する。
generic定数の任意word値を有限値保証と混同せず、D3D12 inlineの動的index禁止と
resource descriptor配列の非対応を取込契約へ明記した。
Vulkanの全Rasterで準備済みProgramを記録前に確認し、未準備のbindingなしProgramもpanicせず容量エラーにする。

Program集合は生涯累積でなく現在の集合に64資産までとし、準備成功後に交換する。
Vulkanのfresh owner／descriptor領域再作成は明示変更境界の費用であり、通常frameから移動した隠れた仕事ではない。
D3D12の結果返却anchorはnative readback枠の早期返却と分け、全消費後も結果ハンドルのDropまで変更を拒否する。
anchorはregistryに保持して通常の結果Dropで解放せず、idle公開前の取得で再準備との競合を防ぐ。
shader／descriptor／資源の所有、GPU完了、native失敗処理を検証cfgへ移していない。

新しいGPU時間比較、他GPU／driver、Linux実GPU、実OOM／Device loss、MRT／一般mip、
Bistro PBR／影／tone mapping／Bloomは、このRaster単体の受入では未実行である。
Vulkanの一般推奨に沿ってpipeline／descriptorを明示準備し、通常frameのnative作成を避けた。
新たなvendor最適化や測定なしの性能優位は主張せず、必須の実GPU CIや時間閾値も追加していない。
表示・自由camera・通常frame時間計測の現在の結果は、冒頭の`renderer-7ft.2.6`を参照する。

## 一般資源のimport・更新・履歴と退役

`renderer-7ft.2.5`。D3D12のbuffer／画像を共有物理registryへ移し、一般Graphのimport/export、
owner・世代・内容revision、refresh/deleteと実GPU完了までの保持を接続した。
Vulkanの既存canonical状態・一般binding・provenanceを同じ利用側fixtureで受け入れた。
二APIのkeyが偶然一致しないよう、runtime ownerはGraphと共通のprocess内採番を使う。
現行契約は[RenderGraph](../design/rendergraph.md#d3d12-buffer-lifetime)、
再実行は[一般資源の受入](../guides/development.md#general-resource-acceptance)を参照する。

Windows x64、Rust 1.99.0、RTX 3060／driver 610.88。同一LUIDを確認し、GPU作業は逐次実行した。

| 確認 | 結果 |
|---|---|
| 共通履歴fixture | 初回／reset／3×7→2×2 resize、CPU待機なしの3段Storage画像＋buffer加算を実行。履歴9出力・1104 byte／API／回を独立期待値と二API間で完全一致。各APIの48変動frameも全画素一致 |
| importと更新 | 画像／bufferの重複import、read-only consumerの複数投入、pending中の両資源更新とdelete、古いsnapshot拒否とrefresh、別backendのkey拒否を確認。importしたvertex/indexとattachment Loadを標準opaqueで描き、両APIで全21 pixelが白 |
| D3D12記録境界 | prepare後の内容更新をsubmitで拒否。未投入cancel、二つ目のexport token取得失敗後の参照・token返却と再投入成功、最終readbackを確認 |
| strict／shipping | Renderer追加検証＋Core/SyncVal／Debug Layer＋GBVと、追加検証・専用native診断を除外したnative Disabled構成を別processで各3回成功。strictは最終native解放後までwarning／error／corruption／callback失敗ゼロ |
| 一般履歴メモリ | 各APIの24 warm-up＋24変動frame。構築／compile／prepare／record／submitからFence完了、readback、旧履歴deleteと全plan/result/metadata返却までRust alloc／alloc_zeroed／realloc／dealloc、VMA host alloc／realloc／free、native作成／破棄は全て0。reset各48回は別計上 |
| 既存回帰 | 二APIの混合Raster／Compute strict/shipping、独立processの両API Compute-only／標準混合メモリが成功。既存厳格nativeライブラリ回帰62件も成功 |
| CPU・品質 | 通常CPU160件、release全feature325件、release二API製品301件（各Rustdoc1件を含む）が成功。全feature／targetと二API製品構成のClippyがwarnings拒否で成功。D3D12単独も診断enabled/disabledのcheck・Clippy・debug/release CPUと独立診断featureを確認 |

一般履歴のメモリ窓はlibtest内で逐次測定し、既存の独立processメモリゲートと併用した。
実行コマンド・UTF-8 LF source hash・raw byteと診断・メモリ結果は
[検証データの`general_resources`](frame-memory.json)に記録する。
strict native回帰の一括指定には明示opt-in必須の性能専用試験も含まれ、環境変数未指定でその1件が拒否された。
正しさ回帰62件は成功しており、この課題で性能専用試験や新しい時間性能比較は行わない。
shippingのゼロ時間Fence待機はVulkan各57回、D3D12各55〜57回のtimeoutを実観測し、再wait後に内容を照合した。
待機失敗／Signal失敗の所有は既存CPU注入、部分native作成失敗とpending退役は既存実GPU回帰を併用する。
実OOM／Device loss/removalは発生させていない。shippingの診断件数0は外部Layer注入の不存在を証明しない。
D3D12は既存COMMON復帰を維持し、新たなvendor最適化・未計測の性能効果やVulkan/VMA推奨の例外は設けていない。
一般Raster shader・blend/depthの`.2.8`と二API表示・通常frame時間の`.2.6`は、それぞれの現在の受入記録を参照する。

レビュー修正`renderer-7ft.2.5.1`で、D3D12も登録資源をdeleteせずworking setを変更できるようにした。
全実行・結果・export付きsubmission／buffer snapshotの返却を確認し、新poolの準備成功後に
canonical registryを移す。登録buffer／画像のkey・内容・provenanceを維持し、新classにない登録画像のview枠も補う。
Graph color画像は初回からRT／UAV対応とし、後から追加したStorage Programも同じ画像を使える。
この固定用途の性能・memory効率は未比較で、理由と代案は[資源管理の契約](../design/rendergraph.md#d3d12-buffer-lifetime)に記す。

修正後の最終ソースでは、strict／shippingの二API受入と既存混合Raster回帰をそれぞれ1回再実行した。
登録3資源＋準備3資源の6-entry上限で2×2／3×2を6回交互に準備し、旧read-only plan、
新Storage Programによる旧HDR画像の読み書き、登録indexの描画と全画素を両APIで確認した。
既存D3D12 native回帰9件（pending寿命、部分作成失敗、provenance容量、graphics/compute/clear等）も成功した。
過剰容量・不正classの準備失敗後にも旧資源・構成を利用できた。
完了済みexport metadataと独立したrefresh bufferの最終cloneによる交換拒否、返却後の成功を確認した。
修正前は交換後の通常返却でRust deallocが4回376 byte発生したが、修正後の6返却窓は
alloc／alloc_zeroed／realloc／dealloc・VMA host・native作成破棄の全イベントが0だった。
通常履歴の24測定frameも両APIで確保／解放0を維持した。resize／Program準備自体は明示変更境界であり、このゼロ計測に含めない。
strictはCore／SyncValとDebug Layer／GBVを確認し、最終native解放後まで問題0。
release CPUは全feature325件、二API製品301件、D3D12単独の製品188件／追加検証189件が成功した。
全featureとD3D12単独enabled/disabled・二API製品のClippy、warnings拒否Rustdoc、HTMLの17page／2036linkと
32 diagramのviewer／theme／mobile／print確認が成功した。
最終ソース8件のhash・raw report・元の再現条件は
[検証データの`general_resources_review_fix`](frame-memory.json)に記録する。従来の`general_resources`は修正前commitの記録として保持する。
実GPUゲートは引き続き`#[ignore]`の明示実行で、CIへ実GPU依存や時間閾値の必須チェックは追加していない。

## 公開三経路のconsumer移行と混合Compute受入

`renderer-7ft.2.7.4`。`frame_loop`と`engine_workloads`の代表構築を
`begin_graph`／`connect_snapshot`→公開`graphs::forward`→利用側post→`select_output`へ移行した。
既存Engine便利APIも同じ公開recipeを使うため、用途のある便利入口は維持する。
`frame_scaling`の古いexample CPU試験も現行の容量・pool・公開構成へ更新した。
ライブラリの同期・寿命・検証配置やshader binaryは変更していない。

[共有fixture](../../crates/renderer/examples/support/standard_fixture.rs)は、標準forward直接、
identity post、利用側post、標準パスからの直接合成という4出力で三経路を比較する。
既存trusted sampled乗算／storage加算shaderを`Graph::dispatch`で接続し、
無効・乗算→加算・加算→乗算への交換・追加加算を選ぶ。3×7のRGBA16F全画素を独立期待値と照合する。
draw数0／1／4／2、tint・行列・Uniform offsetを変え、使わない多段postは通常のcullingで除く。
直接合成には同geometry depth prepassとLoad・等深度描画、標準outputを含める。

Windows x64、Rust1.99.0、NVIDIA RTX3060／driver610.88。GPU作業は全て逐次実行した。

| 確認 | 結果 |
|---|---|
| CPU公開API | 既存標準API10試験に加え、12変動frameで三経路の構築・post選択・不要枝除去・slot返却を確認 |
| workspace・静的検査 | CPU160件、release全feature325件、release二API製品301件（各Rustdoc1件を含む）が成功。frame_loopのexample CPU9件、全feature／targetのcheck・Clippy、fmt、diffとwarnings拒否の全feature Rustdoc生成も成功 |
| 二API実GPU | strict／shippingの別processが成功。旧RGBA8／HDRの17出力を保持し、4case×4出力を追加。各API各3回、33 readback・4096 byte／回を独立期待値とAPI間で照合 |
| 所有・失敗・Engine回帰 | `engine_graph`のstrict／shipping各1試験、既存Engineのstrict8試験が成功。旧新mesh・複数View・短命入力・出力権限・pending保持・cancel・容量拒否を維持 |
| 代表consumer | `frame_loop`はstrict／shipping各48測定frameと12 warm-upで描画・履歴・入力churnを確認。`engine_workloads`はstrict45画像、shipping519画像を全RGBA照合し、大mesh・2View・streaming・pending更新と退役を確認 |
| 混合構成の通常frameメモリ | 両APIで24 warm-up＋24変動frame、4出力・672 byte／frame、3 pending＋3保持結果を確認。構築／compileからnative実行・完了・readback・全slot返却までRust alloc／alloc_zeroed／realloc／dealloc、VMA host alloc／realloc／free、native作成／破棄が全て0 |
| 既存メモリ回帰 | CPU24frame・Vulkan Engine画像48frameと、両API Compute-only各24frameもゼロを維持。Engine画像ではdraw／pass変更、material・行列変更、prepared容量満杯とcancel、結果返却を再確認 |

strictはCore＋SyncVal／Debug Layer＋GBVを実効確認し、最終native破棄後までwarning／error／corruptionゼロ。
shippingはRenderer追加検証・専用診断を除外し、native Disabledを指定した別buildで内容を照合した。
混合メモリの診断baselineも各API別processで同じ範囲がゼロだった。
native resetはVulkan48回、D3D12は記録cancelも含む96回で、作成・破棄とは別計上する。
最大20 draw、vertex/indexとprovenance、11 HDR＋2 D32／frameと両種のProgramを準備境界で予約し、
通常frameの暗黙増設や待機を加えていない。

実行コマンド、source hashとraw報告は[検証データの`standard_composition`](frame-memory.json)に記録する。
一般import／履歴は`.2.5`、独自graphics shader・半透明／blend／depthは`.2.8`、
表示と通常frame時間計測は`.2.6`、PBR／tone mappingはB2／B3に残る。
consumer例の既存計時値を新たな時間性能比較として採用せず、外部Layer注入とdriver内部確保は未観測とする。
Vulkan／VMA一般推奨からの新たな例外は設けていない。

## 公開標準パスと標準グラフ

`renderer-7ft.2.7.3`。公開`standard::passes::{opaque, depth_prepass, output}`と
`standard::graphs::{forward, forward_with_post}`を追加した。標準層は公開Graph／Program／copy APIだけを使い、
Engineの便利経路も公開forwardを呼ぶ。任意depthとcaller指定attachment・Clear／Load／Store、
unlit scene-linear colorのpost境界を公開し、export／readback／submit／presentは部品から分離する。
同geometry prepass後には限定ProgramのLESS_OR_EQUAL＋depth writeなしを使い、0 drawでもdepth Loadを必須にした。
既存meshのLESS＋write、生成時の定数保証、資源pinとnative同期・寿命・容量・失敗処理は維持する。

Windows x64、Rust1.99.0、NVIDIA RTX3060／driver610.88。GPU試験は逐次実行した。

| 確認 | 結果 |
|---|---|
| CPU契約 | workspaceの通常CPU・release全feature・release二API製品構成が成功。公開標準APIの10試験で直接利用、多段post、直接合成、0 draw、未初期化Load／Discard、任意depth、culling、容量、post出力とGraph置換拒否を確認 |
| 二API実GPU | Core＋SyncVal／Debug Layer＋GBVのstrictと、Renderer追加検証・専用診断なし＋native Disabledのshippingが成功。各API各3回、17 readback・1408 byte／回を独立期待値とAPI間で比較 |
| 新規描画内容 | RGBA8／RGBA16Fの同geometry prepass後に色が描け、奥の形状を拒否し、手前のoverlayを描いても元のD32が変わらないことを確認。標準forwardの空描画clear、二段画像copyのpost、HDR出力copyも照合 |
| Engine回帰 | 既存Vulkan EngineのCore＋SyncVal有効8試験が成功。公開recipeへの委譲後も既存描画・入力所有・cancel等を維持 |
| 通常frameのメモリ | CPU24frameは任意depthのforward／post、Vulkan画像48frameはrecipeと直接prepass構成を切り替え、draw数0／1／4／2、複数pending、cancel、容量不足、結果返却を含めて成功。Rust alloc／alloc_zeroed／realloc／dealloc、VMA host alloc／realloc／free、native作成／破棄は全て0。native reset96回は別計上 |
| 静的検査・文書 | 全feature／targetのcheck・Clippy、fmt、diff検査と独立レビューが成功。全feature Rustdocをwarnings拒否で生成。mdBook17ページ・2012ローカル参照、Mermaid32図とviewer／theme／mobile／printの検査も成功 |

native strictは最終破棄後までVulkan warning／errorとD3D12 warning／error／corruptionゼロ。
メモリprobeはprepass＋colorのため最大4物体に8 drawを準備境界で予約し、通常frameで増設しない。
raw報告、実行コマンドとUTF-8 LFソースhashは[検証データの`standard_graph`](frame-memory.json)に記録する。
色／depthの契約と利用例は[現行設計](../design/rendergraph.md#公開標準パスと標準グラフ)を参照する。

この段階のpost二APIfixtureは両API対応の画像copyを使い、利用側Computeの三経路受入は上記`.2.7.4`で追加した。
一般import／履歴は`.2.5`、独自半透明・shader／blend／depth設定は`.2.8`、PBR／tone mappingはB2／B3に残る。
表示・通常frame時間性能、非観測の外部Layer注入やdriver内部確保は今回の保証に含めない。
prepassedもGraphでは保守的depth read/writeを維持し、Vulkan／VMA一般推奨からの新たな例外や性能改善の主張はない。

## 利用側Graphへの入力接続と出力選択

`renderer-7ft.2.7.2`。`FrameSnapshot::connect_graph`と、Vulkan常駐入力向けの
`begin_graph`／`connect_snapshot`／`submit_graph`を追加した。接続だけではRaster・画像・export・readbackを追加しない。
複数snapshot/View、同mesh旧新版、任意の画像/buffer出力を扱い、一画像の便利経路も構成後に`select_output`する。
`prepare_snapshot`の非mesh登録余裕も利用側指定とし、標準color/depth用の二枠を一律に要求しない。
公開標準パス・標準グラフは上記`.2.7.3`で追加し、三経路受入と代表consumer移行は`.2.7.4`で確認した。
一般D3D12 import/履歴は`.2.5`、独自Rasterは`.2.8`に残る。

`RasterParameters::new`で有限値を保証する不変公開型へ統一し、snapshotと利用側が同じ`Graph::raster`を使う。
保証済み定数の再走査とEngine専用のvalidated描画入口を除去し、公開iteratorの実要素ごとに容量を確認する。
接続scratchはuser code中にlockを保持せず、正常・失敗・unwindで返却する。
常駐meshは構築からcompile・submit/cancelまで既存の準備済みleaseでpinし、受理後はRuntimeのGPU所有へ引き継ぐ。
公開したmesh bufferはwriteと直接exportを構築時に拒否し、不変版とindex provenanceを保つ。
同期・GPU完了・native失敗・owner/世代・容量の保証は製品構成にも残す。

Windows x64、Rust 1.99.0、NVIDIA RTX3060／driver610.88でGPU試験と計測を逐次実行した。

| 確認 | 結果 |
|---|---|
| workspace CPU試験 | 通常構成と最適化済み検証有効・無効構成が成功。短命入力・接続のみのculling・容量・不正iterator・有限値生成を確認 |
| 新公開APIのVulkan実GPU | Core/SyncVal有効と製品相当の両構成で成功。snapshotなしCompute/copy、旧新meshの異なる寸法HDR全画素、複数export、未投入cancel・pending削除・出力権限を照合 |
| mesh-onlyの最小registry | 二つのmesh bufferだけのregistryで成功。非mesh余裕0で画像出力なしのreadback、余裕1と過大値の容量拒否を確認 |
| 既存Rasterの二API受入 | Vulkan/D3D12のstrict・shipping両構成で成功。各backend各3回、順序・Load・深度・texture・HDRの9出力を既存期待値と照合 |
| 既存Engine回帰 | Core/SyncVal有効の8試験と、製品相当のpending旧mesh保守退役・無確保試験が成功 |
| 新接続経路のメモリ | CPU24frame、Vulkan画像48frameの可変draw/パス・複数pending・cancel・容量不足・CPU結果返却を含む計測で、Rust alloc/alloc_zeroed/realloc/dealloc、VMA host alloc/realloc/free、native資源・memory作成/破棄はすべて0。native reset96回は再利用として別計上 |
| 静的検査・レビュー | 全feature/targetのClippy、format、diff検査と独立した所有/APIレビューが成功。重大・中程度の指摘なし |

実行コマンド、ソースhash、メモリのraw報告は[検証データの`graph_input_connection`](frame-memory.json)を参照する。
Vulkan Core/SyncValとD3D12 Debug Layer/GBVのstrict回帰は最終native破棄後までwarning/errorゼロ。
製品相当はRenderer追加検証・診断featureを無効にしnative検証をDisabled指定した。
非観測の外部Layer注入やdriver内部確保、表示・通常frame時間性能、一般D3D12常駐入力、履歴は今回の実証範囲に含めない。
本変更は既存の同期・寿命・生成時保証を引き継ぎ、Vulkan/VMA一般推奨からの新たな例外は設けていない。

## 公開APIの三層方針と移行計画

`renderer-7ft.2.7.1`。[公開API方針](../design/implementation-policy.md#public-api-layers)として、
汎用Graph API、同じ公開APIで定義する標準パス、それらから構成する標準グラフを採用した。
Engineによる現行固定構成と移行後の契約を区別し、入力接続・出力所有・標準部品・consumer受入を`.2.7`、
一般import/履歴/退役を既存`.2.5`、独自Raster/blend/depthを新`.2.8`、表示と時間計測を既存`.2.6`へ分けた。
[着手順序](../design/implementation-policy.md#public-api-migration)をBeadsの依存へ反映し、小HDR外部Bloom`.5.1`を`.2.7`後へ前倒しした。

文書と課題計画のみの変更であり、公開API・native実行・shader・検証配置・メモリ保証の実装は変更していない。
独立した設計・作業計画レビューで重大な指摘なし。0 draw時のLoad保持、構築時からの資源pin、課題の親の表記を明確化した。
`bd dep cycles`は循環なし。mdBook 0.5.4でHTML17ページを生成し、ローカルリンク・fragment・資源の検査、
Mermaid 12.3.0 bundle一致、32図の実描画・viewer・theme・mobile・print・外部asset非依存の検査が成功した。
Rust試験・実GPU受入・時間/メモリの再計測は今回実行していない。新APIの対応や性能を今回の文書検査の成果としない。
同期・寿命・Load/Storeと検証済み入力の保証は既存方針を維持し、一般推奨からの新たな例外は設けていない。

## 利用側Compute・多段HDRと画像copyの二API実行

`renderer-7ft.2.4`、統合実装確認点`adb51d1`。利用側の静的なtrusted Compute descriptorを
明示したunsafe境界で取り込み、owned dispatchをVulkan/D3D12の通常実行へ接続した。
Uniform/Storage buffer、sampled画像/sampler、typed storage画像のbindingからアクセスを導出する。
固定dispatchと出力extent由来の2D dispatch、buffer実byte footprintと全域画像writeの保証を分ける。
Graph構築でslot/範囲/format/extent/dispatch/重複subresourceを保証し、不変operandを引き継ぐ。
GPU書込は対象buffer範囲のCPU index provenanceを失効させる。

Vulkanは`prepare_compute_programs`、D3D12は`prepare_graph_working_set`でpipeline/root layoutと
descriptor/native画像の容量を明示準備する。通常frameの作成・増設・暗黙waitを追加しない。
typed format/usage能力、実Device上限、pass間依存、descriptor排他、Device共通sampler上限、
実Fence完了と不明完了時の保持、native失敗処理は製品構成にも残す。
shader本体の範囲外アクセス・race・全域writeの安全性はunsafe取込のレビュー責務であり、
reflection/hashだけの保証へ読み替えない。現行ABIと未対応事項は[設計](../design/vulkan.md#managed-uniform)を参照する。

Windows x64、Rust1.99.0、`CARGO_INCREMENTAL=0`、NVIDIA RTX3060／Vulkan driver610.88。
vendor/device IDとLUIDを照合し、全GPU作業を逐次実行した。
[実行手順](../guides/development.md#owned-compute-acceptance)、入力・実容量・counter・全52 source hashと26実行コマンド、
raw byteのlossless hex表現と4メモリ報告は[検証データの`owned_compute_native`](frame-memory.json)に記録する。

| 確認 | 結果 |
|---|---|
| 共通CPU／二API release製品／release追加検証＋診断 | 143／292／305件＋Rustdoc各1件成功 |
| all-target Clippy | CPU、presentation＋D3D12＋memory-profilingの製品／診断、およびD3D12単独で警告ゼロ |
| 新CPU回帰 | 未初期化storage read、重複View、ABI不足、dispatch/format/extent不一致と容量付き構築のrollback成功 |
| 同じ公開Graphのstrict／shipping | 各1件成功。各API3回、各回4 readback・416 byteを全て独立期待値とAPI間で照合 |
| 既存native回帰 | Vulkan54件、D3D12 8件、共通Raster/bufferのstrict/shipping各1件、engine公開負荷1件成功 |
| ABI・生成物 | SPIR-V/DXIL全13 assetとD3D12 bindings再生成一致、ABIの正例・意味的な改変拒否成功 |
| 文書 | HTML17ページ・1932ローカル参照、Mermaid32表示とviewer/theme/mobile/print検査成功 |

利用側shaderは4×4×1 local size、3×7入力に1×2×1 workgroupsを使う。
sampled画像＋Uniformの乗算から、storage画像＋GPU生成StorageRead bufferの加算を1/2/3段で接続し、
HDR画像copy、FP32 sampleとbuffer dispatch/copyを照合する。Rendererのeffect列挙は増やしていない。
未準備Programと画像class容量不足の拒否を独立に確認し、明示準備/容量変更後のcancel・再試行も成功した。
同じplanを2回使い、最後は同じ入力から別planを作り、公開plan/runtimeを完了観測前に破棄した。
結果消費までの所有保持を確認する試験であり、破棄時にGPUが実行中だったとは強制・断定しない。

strictはCore＋SyncVal／Debug Layer＋GBVの実効と最終native解放後までの診断ゼロを確認した。
shippingは追加検証・専用診断をコンパイル除外し、native `Disabled`を要求して同じ内容を照合した。
診断なしの件数0を、外部Layer注入がなかった証明には使わない。

メモリ窓は各APIの製品／診断baselineを別processで実行した。24 warm-up＋24変動frame、
3 in-flightと3保持結果、5 Program、dispatch総数5/6/7、入力値・Uniform offset・ping-pong bindingを変える。
Graph構築/compile、prepare/record/submit、完了/poll/retire、全readback消費・全plan/result/Graph slot返却までを含め、
全4窓でRust alloc/alloc_zeroed/realloc/deallocの回数と要求byte、native作成/破棄・memory/descriptor確保解放がゼロ。
VulkanのVMA host callbackもゼロ。native reset48／96は準備済み枠の再利用であり、native全呼出しゼロの意味ではない。
startup/shutdownの正の対照と全slot返却を確認した。driver/OS内部確保、時間性能や大規模sceneへ一般化しない。

独立レビューでCompute samplerのDevice共通quota迂回を検出し、既存helperと容量回帰へ統一した。
初回3×5入力ではHDR全画素は一致したが、既存sample shaderのUVがtexel境界へ当たるFP32 sample期待値が不一致だった。
境界依存oracleという推論に留め、driver不具合とは断定しない。3×7へ変えて0.2 texel以上の内部marginを取り、
許容差を広げず全byte照合を再受入した。初回ログも保持した。一般推奨からの新たな最適化例外はない。

CPU/GPU時間計測の拡張は合意どおり`.2.6`の表示統合へ残す。実native OOM/Device loss、他GPU/driver、
Linux実GPU、D3D12 import/export、二API表示、Bistro/Bloomアルゴリズムは今回未実行である。
以下の過去のnative dispatch拒否・画像copy未対応記録は、各節に記した当時の確認点に従う。

## D3D12 Rasterの用途集約と追跡容量

`renderer-68c`、実装確認点`33587ca`。`.2.3`レビューの3件を改善した。
各Raster/AttachmentPassのbuffer用途は該当passのaccess列だけで合成・消去し、全Graph bufferの初期化・走査を除去した。
index provenanceは`provenance_segments_per_resource`（既定64）で操作数から独立して予約し、
既定のsegment枠は208,896→3,264となった。buffer-onlyと完全な不変mesh要約は0を指定できる。
depth clear値はGraph構築で有限値・0〜1・subnormal拒否を保証し、backendでの同じ値の再検査を除去した。
native対応形式・Device適合・容量・同期・実完了・失敗処理は維持する。

Windows x64、Rust1.99.0、`CARGO_INCREMENTAL=0`、NVIDIA RTX3060。
入力・設定・source hash・実行コマンド・連続窓の結果は
[メモリ検証データの`owned_raster_review_fixes`](frame-memory.json)に記録する。

| 確認 | 結果 |
|---|---|
| 共通CPU／二API release製品／release追加検証＋診断 | 138／277／290件＋Rustdoc各1件成功 |
| all-target Clippy | presentation＋D3D12＋memory-profilingの製品／診断、およびD3D12単独で警告ゼロ |
| 新CPU回帰 | 同じbufferのVertex/Index/Uniform合成、一回の適用、次passのIndex-only、clear-onlyを確認 |
| 新実GPU回帰 | 共有bufferのdegenerate drawとclear内容、容量0/1の拒否・cancel・再試行・mesh要約・pin返却がstrict／shipping各成功 |
| 既存実GPU受入 | 共通Raster、3×5可変clear、共通bufferがstrict／shipping各成功。新回帰を含め10 test-config実行成功 |
| 文書 | HTML17ページ・1920ローカル参照とMermaid32表示、viewer/theme/mobile/printを確認 |

strictは最終native解放後までCore＋SyncVal／Debug Layer＋GBVの実効と診断ゼロを確認した。
shippingはRendererの追加検証・専用診断を除外しnative `Disabled`を要求して、同じ期待内容を照合した。
外部Layer注入の不在を診断なしの件数0から証明したとは扱わない。

D3D12のbuffer／Raster窓を製品／診断baselineの4別processで再計測した。
bufferはprovenance容量0、24warmup＋48変動frameと境界ケース、Rasterは容量16、24warmup＋24変動frameである。
Graph構築/compile、prepare/record/submit、GPU完了・poll/retire、readback消費とplan/result返却までを含め、
全4窓でRust alloc/alloc_zeroed/realloc/deallocの回数・要求byteとnative作成/破棄・memory/descriptor確保解放はゼロ。
native resetの198（buffer）／96（Raster）は準備済み枠の再利用である。startup/shutdownの正の対照も確認した。

segment予約枠の減少を常駐byte・時間性能の実測へ読み替えない。
access列の二回走査は追加の使用列metadataを避ける選択であり、密なbinding入力の時間改善は未計測である。
CPU/GPU時間、可変clearの大解像度比較、他GPU/driver、実native OOM/Device lossは未実行。
Vulkan実装は変更せず、共通Raster/buffer受入を再実行した。以下の`.2.3`実装時のD3D12容量・計測記録は当時の確認点に従う。

## owned Raster・画像転送の二API実行

`renderer-7ft.2.3`、統合ソース確認点`ccd838e`。共通owned Graphから、複数draw順、color-only、
depth付きcolor、depth-only、RGBA16Fとdraw別Uniform/画像/samplerをVulkan/D3D12へ実行する。
レビュー済みProgramとattachment構成のPSO、画像/View、descriptorを明示準備し、通常frameでは保持・再利用する。
一般dispatch/画像copy、MRT、native複数mip/layer、D3D12 import/exportと表示はこの受入に含めない。

Windows x64、Rust1.99.0、`CARGO_INCREMENTAL=0`、NVIDIA RTX3060／Vulkan driver610.88。
同じadapterのvendor/device IDとLUIDを照合し、GPU実行を直列に行った。
[実行手順](../guides/development.md#owned-raster-acceptance)、入力・条件・counter・source hash・25コマンドは
[メモリ検証データの`owned_raster_native`](frame-memory.json)に記録する。

| 確認 | 結果 |
|---|---|
| 共通CPU、二API release製品、release追加検証＋診断 | 138／276／289件＋Rustdoc各1件成功 |
| all-target Clippy | presentation＋D3D12＋memory-profilingの製品／診断、およびD3D12単独で警告ゼロ |
| 同じ公開Graphのstrict／shipping | 各1件成功。各API3回、9 readback・704 byteを全て独立期待値とAPI間で照合 |
| 3×5の可変clear | strict／shipping各1件成功。depth0.25→0.75、0/1、color/depthのLoad保持を全画素照合 |
| 既存Vulkan Raster/Compute/composite回帰 | strict10件成功。draw順・index拒否・共有fetch・descriptor不足/再利用・pendingを含む |
| D3D12の部分作成失敗・cancel・pending所有 | strict1件成功 |
| 共通buffer Graph | strict／shipping各1件成功。画像準備不足と未対応dispatchの拒否後にも再利用可能 |
| ABI・生成物・文書 | 管理下SPIR-V/DXIL ABI、D3D12 bindings再生成一致、fmt/diff、HTML17ページとMermaid32表示の検査成功 |

strictは最終native解放後までCore＋SyncVal／Debug Layer＋GBVの実効と診断ゼロを確認した。
shippingは追加検証・専用診断をコンパイル除外しnative `Disabled`を要求して同じ内容を照合する。
外部Layer注入の不在を診断なし実行から証明したとは扱わない。
共有Graphは二回ずつ同じplanを使い、最後の所有試験だけ同一入力から別planを作る。
公開plan/runtimeを完了観測前に破棄し、結果取得まで所有が残ることを確認した。
GraphPoolのbackingは保持し、破棄時点でGPUが実行中だったとは強制・断定しない。

メモリ窓は各APIの製品／診断baselineを別processで実行した。16×16、24warmup＋24変動frame、
3in-flightと3個の保持結果、5Program、draw0/1/2/3、色形式・attachment形状・binding・入力値を変える。
depth clearも1と0.875を切り替え、native fast clearとshader clearを含む。
Graph構築/compile、prepare/record/submit、完了/poll/retire、全画素readback・結果/plan返却を連続窓へ含めた。
全4構成でRust alloc/alloc_zeroed/realloc/deallocの回数・要求byte、native作成/破棄・memory/descriptor確保解放がゼロ。
VulkanのVMA host callbackもゼロ。native resetの48／96は既存枠の再利用であり、native全呼出しゼロの主張ではない。
startup/shutdownの正の対照と全Graph slot返却を確認した。driver/OS内部の確保、時間性能、物理VRAMやWSIまでの保証へ一般化しない。

初回の内容照合は成功したが、D3D12のoptimized clear未指定による警告820/821で厳格受入が失敗した。
固定既定値はnative clear、他の値は準備済み全域triangle/PSOを使う形へ修正し、警告を除外せず再受入した。
追加draw/帯域の交換条件、代案とD32 subnormal clear拒否は[現行方針](../design/implementation-policy.md#d3d12-runtime)に記す。
不明なGPU完了時にもPSOとclear geometryを保持する修正は独立レビューで確認した。
実native OOM/Device loss、他GPU/driver、直接のD3D12 clear A/B、Bistroの影照明・tone mappingは未実行である。
以下の`.2.2`時点のnative拒否記録は当時の範囲であり、今回のRaster/画像対応を反映した現行境界は上記に従う。

## RenderGraph設計ガイドのHTMLとMermaid図

`renderer-chk`。mdBook 0.5.4 / mdbook-mermaid 0.17.1 / Mermaid 12.3.0で、
既存文書と[導入](../design/rendergraph-overview.md)、[設計図集](../design/rendergraph-diagrams.md)をHTMLへ生成した。
現在の実装確認点は`ccd838e`。主要Rust型の共有所有、flat arenaとowned命令、compile、
producer/culling、live hazard、範囲/subresource、native投入、別々の寿命、work/result状態、
importのrevision、binding/stage導出、容量の13図をMermaid原本へまとめた。導入の3図も同じ原本をincludeする。
生成・配布・固定版・ライセンスと更新方法は[文書ガイド](../guides/html-docs.md)を参照する。

`renderer-6vt`で生成HTMLを`build/docs/`、配布ZIPを`build/rendergraph-html.zip`へ分離した。
mdBook設定、検査の既定値、CIの検査・artifact保存先、利用文書とGit ignoreを同じ配置へ更新した。
新しい配置で17ページのローカル参照検査と全32箇所のMermaid実描画検査、ZIPの完全性確認が成功した。
旧配置の文書生成物だけを除去し、Cargoの出力は保持した。

Windows x64で固定CLI配布物のSHA-256照合、npm lockfileの導入、表示用bundleの再生成一致、
生成済み全17ページのローカルリンク・fragment・asset検査が成功した。
隔離Chromiumで13図の32表示箇所（導入3＋図集13＋全章印刷16）を描画し、構文・SVG title/description、
拡大/縮小/原寸/全体表示、Escでの返却、拡大中のPDF印刷、明暗テーマ、390pxの本文横overflowなし、
印刷時の操作ボタン非表示、localhost以外の文書asset要求なしを確認した。
Chromeでも型関係図・シーケンス図の文字と矢印、原寸拡大、Esc/閉じるでの復帰、暗色テーマと章内移動を確認した。
掲載した完全なCPU Rust例は最新実装でcompile/runが成功した。

図はJavaScriptによりSVGへ描画する。表示ライブラリは同梱し外部CDNへ依存しない。
mdbook-mermaidはビルド元mdBook 0.5.0と呼出元0.5.4の版差を警告するが、上記の生成・実描画検査は成功した。
標準検索の日本語分かち書き制限はガイドに明記した。ブラウザー自動化のURL制約で`file:`直接表示は未確認。
CIには固定CLI取得・ハッシュ照合、bundle一致、HTMLリンクと実描画検査、`rendergraph-html` artifact保存を追加した。

Rendererの実装・validation配置・同期・GPU寿命・メモリ方針は変更しない。新たな推奨例外はない。
実GPU受入・性能計測・shader/bindings再生成は文書変更のため再実行していない。

## Program由来bindingとshader stageのowned計画

`renderer-7ft.2.2.5`、実装・統合確認点`52a5d74`。
buffer/ImageView/samplerの型付きbindingをComputeと各drawで共用し、Programから用途・
Vertex/Fragment/Compute stageと実footprintを導く。descriptor全範囲と実アクセスを分け、
命令・binding・定数・View/samplerを準備済みarenaへ所有する。
実際の別VS/FSに結び付いた`textured_mesh()`はVS Uniform16 byteとFS sampled画像/samplerを持つ。
既存の四Compute assetも同じGraph入口を使う。契約は[Graph設計](../design/rendergraph.md#owned-shader-bindings)に従う。

Windows x64 / Rust 1.99.0で、CPU default132件、`vulkan,d3d12` release製品269件・
追加検証/graph診断あり282件と各Rustdoc1件が成功した。新しい公開API回帰8件、
構成例の無効化試験1件と実行、内部の所有/stage対応・shader reflection契約を確認した。
短命入力とdraw別material、選択mip/layer/depthの初期化、producer/culling、WAR/循環、
後半drawのfeedback拒否、foreign資源、slot/種別/容量失敗後の再利用、descriptor末尾の未初期化を確認した。
全target Clippyは`presentation,d3d12,memory-profiling`の診断有無で成功し、fmtも通過した。
固定SDKによるSPIR-V/DXIL再生成一致、SDK不要ABI試験、matrix layoutとVS/PS signatureの不一致拒否も成功した。

CPUの新しい`shader_bindings`窓は24変動frame、1/2/3 Raster段×2draw、
Uniform offset 0/16/32と3mip×2layerのView変更、sampled Compute出力64 byte、取消・不要write・
3plan保持・全返却/resetを含み、製品releaseのRust確保/ゼロ初期化確保/再確保/解放と要求byteは全0だった。
既存4窓も全0で、追加検証ありは別baselineとして確認した。native実行の測定とは区別する。

RTX 3060 / driver 610.88でVulkan strictの正しさゲート53件（WSI込み）と、製品構成の境界1件が成功した。
二API共有Graphはstrict/製品の両方で10pass・308 byteが全一致した。strictは最終native解放後まで診断ゼロ、
製品は追加診断除外/native Requested Disabledを確認し、外部layer注入の不存在証明とはしない。
liveなbuffer dispatch・textured Raster・sampled dispatch計画は拒否し、拒否時のnative作成/解放8eventは全0だった。
Vulkanの取得/再利用数は不変で、culled枝専用の資源も取得しない。
D3D12の画像計画は先行する画像uploadの未対応判定で拒否するため、新shader命令のnative loweringを通した証拠にはしない。
製品Engine48frame、WSI18source/36present・18readbackも再受入し、RendererのRust4event・VMA host3event・
Resetを除くnative8eventは全0だった。WSIのcaller要求は別rowで記録する。

これで`.2.2`のCPU計画・構成/所有の受入を満たす。一般graphicsのnative PSO/二API実行は`.2.3`、
一般native Compute/copyは`.2.4`、BloomアルゴリズムはB4に残る。HDR Graph形式、storage-image Compute、
任意shader取込、複数subresourceを覆うViewは今回の実装範囲に含めない。
Vulkan guidanceの用途別stage/access・descriptor/資源寿命と検証配置を維持し、新しい規約例外はない。
Linux/別GPU・driver実GPU、長時間耐久、実native OOM/loss、時間性能と物理VRAM改善は未確認。
コマンド・生counter・容量・native比較は[機械可読記録](frame-memory.json)の`owned_shader_bindings`にある。

## 明示Programとowned Raster列

`renderer-7ft.2.2.4`、実装・統合確認点`5d7fb6b`。
実在する両APIのmesh資産へ結び付いた`ManagedRasterProgram::mesh()`と借用draw sliceを公開Rasterへ接続し、
従来のsingle-draw操作とEngine内部batchを同じprepared owned coreへ統合した。
Program identity、順序、各80-byte定数とbuffer/attachment参照を保持し、Engineの有限値保証は再走査しない。
空sliceはattachment-onlyへ正規化する。Vulkanは同じmesh shaderとnative所有者を使い、
複数drawでも指定したLoad/Storeを記録する。
契約と未対応形は[Graph設計](../design/rendergraph.md#producer範囲attachment)と[Vulkan設計](../design/vulkan.md#graphのshaderと描画)に従う。

Windows x64 / Rust 1.99.0で、公開API8件・内部所有/解決失敗2件・Program資産契約1件（Vulkan有効時は2件）の新規回帰を含め、
CPU default122件、`vulkan,d3d12`のrelease製品257件・追加検証/graph診断あり270件、各Rustdoc1件が成功した。
fmtと全target Clippyは`presentation,d3d12,memory-profiling`の追加検証/graph診断有無で成功した。
短命sliceの変更後も残るdraw順/定数/Program/View、後半drawの未初期化・foreign buffer・非有限値・容量・解決失敗、
共有Vertex/Index用途、culling、Load/Discard、slot pin/返却を確認した。
頂点stride28・VS定数80 byte・1 RGBA8出力は既存DXIL reflectionとsource、Vulkan実asset対応へ結び付けた。

製品CPUの新しい`owned_raster`窓は24変動frame、0/1/4/2 draw、共有2mesh、任意depth、定数/clear値変更、
呼出元配列の変更、未compile取消、不要な後続write、3plan保持、全返却/resetまで含み、
Rust alloc/alloc_zeroed/realloc/deallocと要求byteは全0だった。既存CPUの3窓も全0を維持し、
追加検証/graph診断ありは別baselineで確認した。CPU rowだけをnative実行の証拠にしない。

RTX 3060 / driver 610.88でVulkan strictの正しさゲート52件（WSI込み）が成功し、
製品構成の境界テストも別に実行した。depth0と青色をseedした後のLoad/Load複数drawは1024 byteの青色を保持し、
旧batchのClear固定へ戻る退行を検出する。既存のdraw順/深度/Compute連携、後半index不正、共有buffer用途も通過した。
CPU color-only Rasterのnative拒否と、その後の正常実行を確認した。
Engine製品48frame、WSI製品18source/36present・18readbackも再受入し、RendererのRust4event・
VMA host3event・Resetを除くnative作成/解放8eventは全0だった。WSI caller要求は別rowに保持する。
二API共有buffer Graphはstrict/製品とも308 byte全一致。strictはCore/SyncValまたはDebug Layer/GBVの実効と
最終native所有者解放後まで警告/error等ゼロを確認し、製品は追加診断除外/native Requested Disabledで確認した。

このmeshはdescriptor bindingを持たない。`.2.2.5`で別Programのbinding/stage CPU計画を接続したが、他のshader/PSO、
D3D12 Raster、depth-only/MRT描画、first_index/base_vertex/多instanceと一般native subresource実行は後続に残る。
index本文の範囲・未知内容・Device上限とGPU寿命は既存native経路で維持し、CPU Programだけから安全を推定しない。
新しい規約例外はない。Linux/別GPU・driver実GPU、長時間耐久、実native OOM/loss、時間性能と物理VRAM改善は未確認。
shader/bindingsは無変更で再生成Verifyは未実行。コマンド・生counter・容量・native結果は
[機械可読記録](frame-memory.json)の`owned_raster`にある。

## attachment配列と画像subresourceのCPU計画

`renderer-7ft.2.2.3`、実装・統合確認点`f290cef`。
`ImageDesc`のmip/layer数と同じ論理画像に属する単一subresource View、
描画命令なしの`attachment_pass`のcolor配列/optional depthを追加した。
短命sliceの所有、各mip/layerの初期化・producer・RAW/WAR/WAW・culling・全域export、
Load/Store/Discardとowner/extent/format/alias/容量失敗後の再利用を確認した。
旧`clear_attachments`入口は置換し、固定mesh Rasterは従来の形だけを扱う。
CPUで扱える構成とnative実行範囲は[Graph契約](../design/rendergraph.md#画像subresourceとowned-attachment配列)に従う。

Windows x64 / Rust 1.99.0で、公開API8件・所有/限定lowering2件・資源契約4件の新規回帰を含め、
CPU default111件、`vulkan,d3d12`のrelease製品245件・追加検証/graph診断あり258件、各Rustdoc1件が成功した。
fmtと全target Clippyは`presentation,d3d12,memory-profiling`の追加検証/graph診断有無で成功した。
新しいCPUメモリ窓は24変動frame、3mip×2layer、color-only/depth-only/MRT、Load/Store、
clear値変更、未compile取消、culled枝、3plan保持、全owner返却とslot resetまでを含み、
製品releaseのRust alloc/alloc_zeroed/realloc/deallocと要求byteは全0だった。
従来のsnapshotとowned Computeの窓も全0を維持し、追加検証ありは別baselineで確認した。
準備境界のattachment arenaによる保持容量は増える。counterゼロから常駐量・時間性能の改善は推定しない。

RTX 3060 / driver 610.88で、Vulkan strictの正しさゲート52件（WSI込み）が成功した。
新しいnative境界テストはstrict/製品で別実行し、単一画像cacheと同じbase extentを持つmulti画像の取得・準備拒否、
liveなcolor-only/depth-only/MRT拒否、culled multi枝の非取得、従来clearの赤色64 byteを確認した。
Engineの製品48frameとWSIの製品18source/36present・18readbackも再受入し、RendererのRust4event、
VMA host3event、Resetを除くnative作成/解放8eventは全0だった。WSIの同期的なcaller要求は別rowに保持する。
二API共有Graphはstrict/製品とも10pass・308 byteが全一致した。
strictはCore/SyncValまたはDebug Layer/GBVの実効と、最終native所有者解放後まで警告/error等ゼロを要求した。
製品は追加検証・専用診断を除外してnative Requested Disabledとし、外部layer注入の不存在証明にはしない。

native画像は今も1mip/1layerのみで、新attachmentのloweringは従来の1 RGBA8＋D32・両方Clear/Storeだけである。
一般owned Raster・graphics Program/PSO、複数subresourceを覆うViewとnative実行、Bistro描画は後続に残る。
同期/寿命/validation配置/shader信頼境界の新たな例外はない。Linux/別GPU・driverの実GPU、長時間耐久、
実native OOM/loss、時間性能は未確認。shader/生成bindingsは無変更のため再生成Verifyは未実行とした。
実行コマンド・生counter・native境界の結果は[機械可読記録](frame-memory.json)の`attachment_subresource_cpu`にある。

## 管理下buffer Computeのowned計画

`renderer-7ft.2.2.2`、実装・統合確認点`65893fa`。
`Graph::dispatch`は管理下uniform/storage readbackのProgram identity・workgroup数と、
manifest順の論理buffer descriptorを準備済み領域へ所有する。
shader実footprintを別契約として保持し、32-byte descriptorでも読書き・初期化は先頭16 byteだけに限定した。
所有・保証境界・制限は[Graph契約](../design/rendergraph.md#owned-shader-bindings)に従う。
一般Computeのnative実行、native Program/Bindingsのimport/保持、MRT/subresourceのnative実行、一般Rasterは未実装のままである。

Windows x64 / Rust 1.99.0で、公開API8件と内部契約2件の新規回帰を含め、
CPU default97件、`vulkan,d3d12`のrelease製品231件・追加検証/graph診断あり244件、各Rustdoc1件が成功した。
短命入力後の所有、descriptor tailの未初期化拒否、直前producerとculling、RAW/WAR/WAWの逆順制約拒否、
同bufferの非重複実範囲、slot/foreign・旧世代handle/range/alias/dispatch不正、途中容量失敗後の再利用とplan pinを確認した。
fmtと全target Clippyは`presentation,d3d12,memory-profiling`の追加検証/graph診断有無で成功した。
shader・生成bindingsは変更せず、既存SPIR-V/DXIL reflectionのABI一致・不一致拒否スクリプトも成功した。
shader/bindings再生成Verifyは今回未実行である。

CPUメモリfixtureへowned dispatchの別計測窓を追加した。24変動frame、consumer数1/2/3、
offset 0/16/32と入力値変更、culled枝、未compile取消、3plan保持、全owner返却とslot resetを含め、
製品releaseのRust alloc/alloc_zeroed/realloc/deallocと要求byteは全0だった。
従来のsnapshot/build/compile/return窓も全0を維持し、setup/shutdownの正の対照を確認した。
追加検証/graph診断ありreleaseは別baseline実行で正しさとcounterを観測した。
CPUのみのnative counterゼロをnative dispatchの証拠には使わない。
[容量・生counter・コマンド・native境界の結果](frame-memory.json)の`owned_compute_cpu`へ記録した。

RTX 3060 / driver 610.88で、二API共有buffer Graphをstrictと製品の別runで逐次実行した。
両backendのlive dispatch拒否、その後の実行枠再利用、culled dispatch枝の非取得を確認し、
既存10passの308 byteは各backend・CPU期待値間で全一致した。
strictはCore/SyncValとD3D12 Debug Layer/GBVの実効、最終native所有者解放後まで警告/error等ゼロを確認した。
既存管理下buffer資源/版・producer→StorageReadのnative回帰もstrict3件・製品3件を逐次実行し成功した。
これは既存private実行fixtureの回帰であり、新しいGraph dispatchの実GPU実行ではない。
製品は追加検証・専用診断を除外しnative Requested Disabledで確認した。外部layer注入の不存在証明にはしない。
同期・資源寿命・shader信頼境界・準備容量・validation配置の一般推奨から新たな例外はない。
Linux/別GPU・driverの実GPU、長時間耐久、実native OOM/loss、一般dispatchの二API内容照合は未確認であり、
親`.2.2`と後続`.2.3`/`.2.4`を完了したとは扱わない。時間性能・物理VRAMの改善も主張しない。

## 通常フレームのメモリ規約と実装

`renderer-dzz` / `renderer-7ft.2.2.1`。従来の実装確認点は`43ef7e9`。
追加compile改善の`d633021`でも下表の5経路を製品releaseで再受入し、全ての通常窓でゼロを維持した。
最新の容量・counter・実行コマンドは[ソースレビューの証拠](source-review.json)の
`rendergraph_efficiency_improvements.compile_time_reduction.current_memory_reports`、旧基準と負の対照は[従来の計測](frame-memory.json)にある。
[規約](../design/implementation-policy.md#steady-state-memory)に従い、準備済み資源・容量内の通常frameでは
Rendererの汎用heap alloc/alloc_zeroed/realloc/deallocと、Renderer発行のnative資源/メモリ作成・破棄を行わない。
入力・draw数・pass構成を毎frame変え、構築/compileからGPU/present完了、CPU結果消費/返却まで確認した。
固定plan再生だけの別入口や、元の確保する通常入口は残していない。

| 製品releaseの計測 | 通常窓 | Rust 4 event | VMA host 3 event | native作成/解放8 event |
|---|---|---|---|---|
| CPU snapshot/build/compile/return | 24frame、3owner保持 | 0/0/0/0 | 対象外 | 対象外 |
| D3D12 buffer | 48frame＋未消費結果破棄/切離し退役 | 0/0/0/0 | 対象外 | 全0 |
| Vulkan buffer | 48frame＋未消費結果破棄 | 0/0/0/0 | 0/0/0 | 全0 |
| Vulkan Engine Raster/Compute/Composite | 48frame、3pending/3held結果 | 0/0/0/0 | 0/0/0 | 全0 |
| Vulkan WSI | 18source/36present、18readback（同時保持6）、3pending/3held結果 | 0/0/0/0 | 0/0/0 | 全0 |

Rust欄はalloc/alloc_zeroed/realloc/dealloc、host欄はallocate/reallocate/freeの順で、要求byteも全0だった。
既存枠のResetは許可し、descriptorの暗黙freeとは区別する。
全ての通常owner Dropを窓内に含め、setup/shutdownの正の対照でhook稼働を確認した。
CPUのみのnativeゼロは証拠に使わない。[機械可読の結果・容量・実行コマンド](frame-memory.json)と
[再実行手順](../guides/development.md#通常フレームのメモリ計測)を参照する。

共通はGraph/Snapshot/ImportRangesの準備済みslot、flatなaccess/draw/name/uploadと解析workspaceを使う。
論理Graphの区間は予約済みAVL node arenaへ移行し、offset順による全配列の後続移動を除去した。
`Graph::compile(self)`で同じbackingを凍結し、consumerが終わるまでpinする。全accessの一時Vecも`.2.2.1`で除去した。
Vulkanはnative実行/descriptor/stagingとwork/result/exportを別寿命のpoolへ分け、D3D12もnative実行と独立resultを事前作成する。
通常の容量不足では増設・暗黙waitをせず、必要な検査・同期・GPU/present/CPU寿命・native失敗処理を維持する。
資源変更に伴う旧meshの最後のsource/登録/Map nodeと旧Swapchainの破棄は明示保守へ分け、
通常frameの作業枠・readback返却はそのまま進める。旧meshのprepared pinが後から戻るcaseと、
回収可能な旧Swapchainを保持したまま18sourceを描くcaseでもゼロを確認した。

WSIは通常epoch全体を囲み、同期的なwinit/WindowのRust要求を別rowへ示す。
この独立fixtureはshipping RendererのRust worker/native Rust診断callbackを持たない。
native/host callbackはevent pump中も含むepoch全体を数え、callerとRendererのnative/host rowは重複し得る。
資源変更/resize/VSync/suspend/取得後取消、明示保守、追加1sourceのpending終了は別境界に記録した。
旧世代の実破棄を通常pollへ戻さず、完了観測と枠返却は通常経路に維持した。

以下は`43ef7e9`時点の受入範囲である。Windows x64/Rust 1.99.0、RTX 3060/driver 610.88で、開発release CPU230件/製品release CPU214件と各Rustdoc1件、
全target Clippyの有効/無効構成、fmtが成功した。strict Vulkan51件（WSI以外）、WSI1件、
32 CPU asset/GPU常駐3のstream/churn/複数View/別renderer/replay受入をstrict/製品で確認した。
二API共通の10pass Graphはstrict/製品とも308 byte全一致。D3D12 strict寿命1件・管理下Program2件も成功した。
strictはCore/SyncValまたはDebug Layer/GBVの実効と最終native所有者解放後までwarning/error等ゼロを維持する。
製品測定は追加検証なし・native Requested Disabledで行い、外部Layer注入の不存在を証明したとは扱わない。
固定D3D12 bindings再生成VerifyのSHA-256は`c179861321f92adb42ccd014b0284a9d8e0e37e02f5cc21c9c7e51cc1b3e7f1c`。
shaderは無変更で今回の再生成Verifyは対象外とした。

native hookの独立監査では通常frame/退役/presentの未計測factory/drop/resetはなかった。
VMA host rowはVulkanへ転送されたcallback要求も含み、VMA/driverの起源を分けない。
Vulkan device memoryは成功通知、D3D12 committed resourceは暗黙heap要求、descriptorはset/要素数で、物理割当数として加算しない。
callback外のdriver/OS/native内部、D3D12 bootstrap/CPU shader blobとtest専用旧factoryは未観測であり、
後者を通常の準備済みframe経路で呼ばないことを別に監査した。
別GPU/driver・Linux実GPU・長時間耐久・実native OOM/loss・時間性能/物理VRAM改善は未確認である。
準備容量による常駐量増加、木の更新とprovenance等の配列操作は代償として残り、ゼロからCPU/GPU高速化を推定しない。

CPUの移行前基準値`117ae5b`は同じ24変動frameで2,502/24/62/2,526回だった。
同構成のrequire-zeroが旧基準を拒否する負の対照も保存した。
旧合成compileの37,953要求は別入力のalloc/realloc要求で、deallocや通常frame全体の証拠ではない。
`.2.2`の後続owned命令・MRT/subresource等まで実装完了したとは扱わない。
実装commitの[CI 43674843105](https://github.com/tsukushibito/renderer/actions/runs/43674843105)は全10job成功、初回、4分53秒だった。

## Graph・表示・descriptorの効率整理

`renderer-0dm`の2.2レビュー修正は`9f65b97`で統合確認した。
Rasterのvertex/index集約は使用resource列だけを初期化・sort・列挙し、各passでの全resource走査を除去した。
同一bufferの両用途、最大index範囲、従来のaccess順を保ち、後半draw/attachment/access容量失敗後の
別meshによる再試行と、pass間・slot再利用の回帰を確認した。追加準備領域はresource上限×usize、
既定x64で2 KiB/slotである。密な入力のsort費用を含む実時間性能は未測定。
D3D12は製品の`bounded`経路に重複する未使用のtest用buffer計画・記録を削除し、
実buffer一つの作成後失敗fixtureと、管理下shaderのprivate実GPU受入を維持した。

Windows x64 / Rust 1.99.0でCPU133件、`vulkan,d3d12`のrelease製品270件・
追加検証/graph診断あり283件と各Rustdoc1件、fmt、全target Clippyの
`presentation,d3d12,memory-profiling`の追加検証/graph診断有無が成功した。
CPUの既存5計測窓は各24変動frameで、両構成のRust確保/ゼロ初期化確保/再確保/解放と要求byteが全0だった。
追加検証ありは別baselineであり、CPU窓をnative実行のメモリ計測とは扱わない。

RTX 3060 / driver 610.88でVulkan batchの順序/深度/Compute内容、後半不正index、
共有Vertex/Indexの厳格3件、製品構成のattachment境界1件が成功した。
D3D12は実buffer部分作成失敗/取消/pending寿命の厳格1件、保持した管理下shader fixtureの
strict2件・製品2件が成功した。二API共通buffer Graphも別processのstrict/製品で各10pass・308 byte全一致。
strictは最終native解放後まで警告/error・callback失敗ゼロ、製品は追加診断除外/native Requested Disabledを確認した。
外部layer注入の不存在、実OOM/loss、別GPU/driver、時間性能・物理VRAM改善は未確認。
同期・GPU/CPU寿命・容量/native失敗の保証とvalidation配置を維持し、新しい規約例外はない。
shader/生成bindingsは無変更で再生成Verifyは実行していない。コマンド・counter・確認点は
[機械可読記録](frame-memory.json)の`graph_review_cleanup`に記録した。

`renderer-l3g`と、`renderer-6de` / `renderer-ygu` / `renderer-xqo` / `renderer-0g6` / `renderer-9nb` / `renderer-agu`。
追加compile改善は`2b392d3`との比較で、統合実装は`d633021`。配列実装`1fbe51c`も同じharnessで再測定した。
[生sample・public API/Engine harness・実行コマンド](source-review.json)の
`rendergraph_efficiency_improvements.compile_time_reduction`へ記録した。途中のpipeline単体と初回計測も残す。

明示依存をpassごとの予約済みlinked arenaへ追記し、後続passのoffset更新を除去した。
内容/hazardの区間はresourceごとの予約済みAVL frontierへ移し、無関係な後続segmentの移動を除去した。
producer、未初期化read拒否、culling後の再解析、RAW/WAR/WAW、容量上限は維持する。
構築時と追加検証のoverlap判定、Raster/clearのtargetとclear値検査は共通helperへ整理した。
容量不足は11項目とGraph/import slot等の種別・上限・判明した必要数を、heap確保なしで返す。

追加改善では、全辺が前向きなら従来と同じ順序を直接出力し、CSR/ready heapの構築を省く。
全passが宣言順のまま残る場合だけ初回dataflowの内容を再利用し、同じwriteのreplayを省く。
cullingや並べ替えがあれば再計算し、hazard・容量拒否・循環・診断lifetimesは維持する。
区間nodeは96/104→64/64 byte、reader履歴は32→24 byteへ縮小した。
既知の隣接位置を分割に使い、高さ不変で再balanceを止め、全域置換では古い木をまとめて回収する。
固定planのcacheや診断を外して見掛けの時間だけを減らした比較ではない。

Windows x64 / Core i5-14600KF / Rust 1.99.0、`rustc -O`のfeatureなしで同じharnessを使い、
計測processだけを同じlogical CPU（mask `0x1`）へ固定した。100msの共通process warm-upに続き、各入力は8回warm-up後9回、表は中央値である。
clock/priorityは固定せず、前回→pipeline単体→今回→配列参考の順で別processを逐次実行した。
初回の小規模caseに見えた悪化は共通warm-up後に再現せず、初回sampleも保存した。周波数・schedule・背景負荷の寄与は分離していない。
時間はGraph構築後のcompileのみで、構築・plan返却は含まない。全寿命の確保ゼロは別の連続区間で確認する。

| CPU入力 | 配列参考 `1fbe51c` | 前回 `2b392d3` | 今回 `d633021` |
|---|---:|---:|---:|
| 8,192passの疎な依存chain追加のみ | 28.669 ms | 0.0276 ms | 0.0279 ms |
| 8,192個の穴を挟むwriteのcompile、offset降順 | 120.549 ms | 4.117 ms | 1.202 ms |
| 同、固定seedの順序 | 62.719 ms | 8.251 ms | 2.905 ms |
| 同、offset昇順 | 1.638 ms | 5.085 ms | 1.414 ms |
| 8,192個の隣接write＋全域read、昇順 | 1.470 ms | 2.883 ms | 1.188 ms |
| 同、固定seedの順序 | 41.913 ms | 5.517 ms | 2.574 ms |
| 8,192passのdisjointな後向き依存chain | 20.177 ms | 2.579 ms | 1.502 ms |
| 8,192個のwriteを全域writeで上書きして除去 | 0.439 ms | 1.184 ms | 0.398 ms |
| Engine 128mesh/1,024draw＋2Compute/readback、260pass | 0.0842 ms | 0.0192 ms | 0.0121 ms |
| Engine 1,024mesh/1,024draw＋2Compute/readback、2,052pass | 3.8434 ms | 0.1600 ms | 0.0924 ms |
| 同、未使用Compute branchも追加して除去 | 3.8383 ms | 0.1558 ms | 0.1137 ms |

前回悪化した昇順・広いreadも今回の同条件では短縮した。Engine入力は小さなtriangleの共有/別meshであり、
重いgeometry・Bistro・実アプリ全体・GPU時間・FPSの改善を示さない。16/128passの小規模入力と不要branchも測定した。
1mesh/8,192drawをbatch化した6passのEngine caseは約0.5〜0.7µsで、100nsの時計粒度付近の差を改善・退行と断定しない。
全入力での速度を保証せず、壁時計の固定CI閾値も追加しない。
同じCPUメモリfixtureの準備時Rust要求byteは配列114,504→前回153,240→今回122,088となった。常駐/peak/VRAMの値ではない。
通常24frameの全counterゼロは維持した。

Vulkanは最後に確立したreadのexact stage/access pairを累積readerとwriterから分離し、
同じbuffer readのCPU回帰で依存生成8→1を確認した。新scope、write、layout変更は必要依存を保持する。
Snapshotごとに16 byte増える。submit/retire/reset等は予約上限全体ではなく当該Graphのresource prefixだけを走査し、
大小のGraph再利用と取消・途中失敗を確認した。D3D12 mapの二重初期化も除去した。

統合後はCPUのみ87件、開発release245件、製品release229件と各Rustdoc1件、fmtと全target Clippyが成功した。
RTX 3060 / driver 610.88でstrict Vulkan52件（WSI込み）、D3D12寿命1件、二API共有bufferのstrict/製品を逐次実行した。
共有bufferは各backend308 byte全一致。strictはCore/SyncValまたはDebug Layer/GBVの実効と、最終native解放後まで警告/error等ゼロを要求する。
製品の同一reader、並べ替えreadback、mesh描画、旧mesh退役、5経路のメモリ・表示受入も成功した。
適用した同期・所有・入力保証の一般推奨からの新しい例外やpolicy変更はない。
Linux/別GPU・driver、Bistro全体、実OOM/loss、長時間耐久、GPU性能・物理VRAM改善は未確認である。
shader/生成bindingsは無変更で再生成Verifyは今回未実行。既知の未再現起動異常`renderer-0pb`の解決も主張しない。

`renderer-agv`、比較元`413ff7d`。read/exportのproducerを最終依存集合へ直接追加し、
readごとの一時BTreeSetと取り込み直しを除去した。途中に未初期化の穴があれば解析全体を失敗にし、
producer/culling/hazard/live cycleと最終内容再計算は維持する。
この比較点では全accessの解析用一時Vecは未変更だった。現行では上記`.2.2.1`の準備済みworkspaceへ移行済みである。

表示は同queueでcanonical scopeが正確にFRAGMENT/SAMPLED_READ、layoutがSHADER_READ_ONLYのときだけ
source画像barrierを省き、Swapchain画像barrierと呼出しは残す。新write/scope/layoutでは必要依存を記録する。
previousの保持をscalar scopeへ狭め、唯一のowned Snapshotをnextへmoveして2回目のcloneを除去した。
内容revision/初期化/provenance/writer、submit受理時公開、source leaseとGPU/present退役を維持する。
D3D12はtable用rangeだけを作り、buffer-only bindingの全None table配列を除去した。
mixed root index、native pointerの安定、全資源の強保持と部分失敗cleanupは維持する。
適用した同期・所有・境界保証の一般推奨からの新たな例外はない。

[同一の既存CPU harnessと結果](source-review.json)でrelease/default featureなしを比較した。
2,048資源/4,096passのcompileは確保要求40,001→37,953回、総要求byte 7,125,062→6,912,070。
8,192drawのframe構築は18回/1,706,449 byteで不変だった。allocatorのalloc/realloc要求であり、
常駐量・peak・CPU時間・GPU性能の改善率を示すものではない。

Windows x64/Rust 1.99.0でfmt、CPUのみ/製品/開発の全target Clippy（`-D warnings`）、
CPUのみ通常71件、製品release通常192件、開発release通常203件と各Rustdoc 1件が成功した。
CPU回帰は表示の省略・失効条件を1件追加し、D3D12は既存試験をinterleaved root順へ変更した。
producerは既存のhole/producer/export/culling回帰を活用し、同じ契約の試験を複製していない。

RTX 3060/driver 610.88でreleaseの適用実GPU8件を逐次実行した。
strict/製品それぞれでWSI 1件、D3D12 managed 2件、二API共有buffer Graph 1件が成功した。
WSIは同一出力2回のaccepted presentを18出力分確認し、間に明示GPU waitを追加せず、
内容照合、resize/VSync/取得後取消、pending終了と最終Window/native所有者の解放を確認した。
strictはCore/SyncValまたはDebug Layer/GBVの実効と最終native解放後まで警告/エラー等ゼロを維持する。
製品構成は追加検証/専用診断を除外し、releaseとnative Requested Disabledで同じscenarioを実行した。
外部Layer注入の不存在を証明したとは扱わない。独立レビューに未解決指摘はない。
正式ログは`.tools/agv/gates/`。実行filterは[開発ガイド](../guides/development.md)と機械可読の結果を参照する。
shader/生成bindingsは無変更で今回再生成Verifyは実行していない。
全Graph/transfer実GPUの再受入、Linux/別GPU/driver、Bistro全体、時間性能/長時間耐久、実native OOM/lossは未実行。
既知の未再現native起動crash `renderer-0pb`の解決も主張しない。

<a id="managed-uniform"></a>

## 管理下Programと資源bindingの二API実装

`renderer-7ft.2.1.7`。Uniform/StorageとRGBA8/RGBA16F/D32画像/View/samplerの契約を維持し、
StorageRead bindingとreadonly storageの16-byte copyを両APIへ追加した。
`Binding`/`BindingResource`がbuffer/View/samplerを一つのcomplete collectionで扱い、
旧buffer専用binding APIを置換した。既存の固定ProgramKindは増やしていない。
親`renderer-7ft.2.1`の作成契約はこの範囲で実証した。owned命令・初期化/hazard/subresource計画は
`.2.2`、graphicsのstage/vertex/attachment/PSOとRaster実行は`.2.3`へ残す。
[作成保証とVulkan所有](../design/vulkan.md#managed-uniform)、
[D3D12のroot/tableと保証](../design/implementation-policy.md#d3d12の管理下programとuniform)、
[再実行](../guides/development.md#管理下uniform-programの作成と受入)を参照する。

safeなshader入口はレビュー済みのuniform/storage-read/sample/sample-float readback 4本である。
新`storage_read.comp`はset0/binding0のreadonly SSBOの先頭16 byteを読み、
set1/binding0のwriteonly出力へ16 byteを書く。DXILのraw SRV `t0/space0`とUAV `u0/space1`、
SPIR-VのNonWritable/NonReadableを実dumpでも確認し、manifestとreflectionへ結び付けた。
両sampled assetはseparate texture2Dとsamplerを`textureLod`で実際に使い、
DXILでも`t0/space0`と`s1/space0`をactiveに保持する。`sampled_readback`はRGBA8へpackして16 byte書き、
`sampled_float.comp`は4 sampleの全RGBAをFP32 bit列の64 byteとしてStorageへ書く。
D32でもこの既存shaderを再利用する。
SPIR-VはShader capabilityと32-bit floatのみで、shaderFloat16や16-bit演算型を要求しない。
生成時のSPIR-V/DXIL reflection照合はslot/type/count/member offset/size/local sizeに加え、
float2D image、sampler、writeonly出力と余分な資源を確認する。
float出力のarray 4、literal count、stride 16、offset 0、block size 64も照合する。
固定SDKの全9本SPIR-V/DXIL再生成Verify、SDK不要の改変拒否（pwshとWindows PowerShell 5.1）、
D3D12 bindings Verifyが成功した。既存8shaderの43 source/binary/HLSL/reflectionファイルは比較元`35ae8f0`と不変である。
storage-read SPIR-Vは`10aa5b93fa3e89a5e9d7c4b32921e4edac215ddc64c1f33dd45cc08d446d4398`、
DXILは`4b249dc0d1728365f867e2219cc556ef1f57141ed1b99ae37d60cb3457a37c6a`。
当該管理下Programの作業点では生成bindingsへroot SRV記録に必要なAPIを追加し、固定generatorとのSHA-256一致
`fbb5d0a887223679910c3e2baa7d0abf7e3c82d52cacc298456d18a195d3564d`を確認した。

作成境界で全slotの欠落/重複/余剰、resource kind/用途、owner、論理buffer範囲とDevice alignment、
実format/usage/extent対応、View範囲を保証する。画像はmip/layer/sample各1、未初期化作成であり、
暗黙upload/submit/waitを持たない。Viewは画像を保持し、bindingはProgram、buffer/範囲、
View/Image、samplerとdescriptor heap/setを強保持する。不変なslotと資源metadataを記録時に再走査しない。
入力を直接参照するlazy slot解決を維持し、検査専用Vec、選択済みslotの再照合、別のslot再探索を増やしていない。
全入力検査に成功してからnative descriptorを一度具体化し、失敗時は途中の参照を解放する。
Vulkanのsampler生存数上限は全旧/新作成箇所でDevice単位予約/失敗返却/native破棄後返却へ揃えた。
D3D12のtexture/samplerはshader-visible SRV/SAMPLER heapのtable、bufferはroot CBV/SRV/UAVを使う。
native容量/失敗・同期・実Fence完了・GPU寿命は製品に残す。
公開Image/Storageの作成成功は内容初期化を意味せず、各shaderが書いた16または64 byteだけをreadbackする。
色のImageとViewは同じ実formatを使い、実DeviceのRGBA16F対応はRGBA8と区別して作成境界で保証する。
D32のVulkan Image/Viewは`D32_SFLOAT`とdepth aspect、D3D12は`R32_TYPELESS` Imageと
`D32_FLOAT` DSV/`R32_FLOAT` SRVを使い、attachmentとsampledの実能力を保証する。
View用途を作成時に固定し、色のdepth attachmentとattachment Viewのsampled bindingを拒否する。
`preferred_depth_clear`は後続clearの予定値であり、作成時の初期化ではない。D32の`None`は合法で、
色への`Some`、NaN/Inf、[0,1]外とsubnormalを共通作成境界で拒否する。
値の検査を記録へ重ねず、不変Image descriptorから実clear値を使う。

StorageReadは同じStorage用途のdescriptorであり、Vulkanはread/write双方をSTORAGE_BUFFER数へ合算する。
D3D12はraw root SRVの16-byte alignmentと論理範囲を作成時に保証する。
private prepareでproducer/consumerのexact asset、同じinput版・先頭offsetと実際の16-byte write/readを
一度結び付け、input/outputを各一度leaseする。descriptorのsize全域を初期化保証とは扱わない。
同bufferは別byte範囲でもprivate実行を拒否する。公開binding作成へalias全slot走査を追加しない。
Vulkanのproducer COMPUTE write→consumer COMPUTE read barrier、D3D12のUAV→NON_PIXEL transitionを使い、
2 dispatch間にCPU待機やcopy/readbackを挟まず、最後のconsumer outputだけをreadbackする。
再利用で前consumer read→次producer writeの実行順も保つ。
asset identity照合とproducer/canonical slotの追加metadataは試験専用であり、通常libraryへ追加しない。
D3D12の既存ManagedProgram保持は通常のlayout取得に必要であり、この除外の主張には含めない。

private upload/dispatch/readbackと入力色、Image/Storageの排他lease/stateは`cfg(test)`に限定する。
通常buildにCPU検査用画素payloadやprivate executor metadataを保持しない。
初回uploadした旧画像と別に作った新画像を保持し、旧画像再利用時は再uploadせず旧内容をsampleする。
VulkanはCOPY→sampled readとStorage write→copy→host依存、D3D12は実GetCopyableFootprintsの
row配置とCOMMON→COPY_DEST→NON_PIXEL_SHADER_RESOURCE→COMMONを確認した。
取消で未初期化を公開せず、pending中の競合を拒否し、不確定失敗では既存Retainedと所有連鎖を保持する。
Vulkan初回画像遷移の既存COPY execution対策も維持する。
D32は転送uploadせず、Vulkanのdepth-only Dynamic Rendering load CLEAR/store STORE、
D3D12のDSV clearで初期化する。Vulkanは両depth stageのwrite→compute sampled read依存と
layout遷移をrendering scope外で記録する。D3D12はCOMMON→DEPTH_WRITE→
NON_PIXEL_SHADER_RESOURCE→COMMONへ遷移する。attachment ViewとDSV heapもWork/Retainedに保持する。
先行D3D12 strict試験ではreadback一致後にclear値未指定の警告821を3件観測し受入失敗とした。
作成時のpreferred clear値をoptimized clear値へlowerして修正し、警告の例外扱いは追加していない。
この値指定の性能効果は測定していない。

Windows x64/Rust 1.99.0でfmt、CPUのみ/製品/開発の全target Clippy（`-D warnings`）が成功した。
CPUのみ通常71件、製品release通常191件、開発release通常202件と、各構成のRustdoc 1件が成功した。
Rustdocは両backendの公開Image/View/Sampler/binding作成例をコンパイルする。
共通契約、shader/ABIと両backendの作成/同期/所有の独立レビューで修正必須の未解決指摘はない。
境界値の組合せは共通CPUへまとめ、native試験での同じ拒否classのmatrixを形式ごとに複製しない。
共通CPU契約は既存test内へまとめた。UniformとStorageReadのbuffer-copy ABI checkerを共有し、
既存Uniformの改変拒否を維持したまま新negativeをreadonly/SRV register/raw read-only種別の4件へ絞った。

RTX 3060/driver 610.88でreleaseの実GPU試験を逐次実行した。
StorageReadは既存uniform producerから同Workで初期化した旧版/新版の16 byteを別Storageへcopyし、
非零offsetの全byte一致を確認した。producerなし、不一致input/offsetと同buffer別範囲を拒否し、
cancel/busy/pending/完了後reuse、両Program/Bindings/input/outputの最終解放を共有fixtureで確認した。
RGBA8は既存の2x2非対称入力をnearest/clampで読み、公開272-byte Storageのoffset256からの16 byteが一致した。
RGBA16Fの旧入力はrow-major成分 `[-2,-1,-0.5,-0.25,0,0.25,0.5,1,1.5,2,3,4,8,16,32,64]`、
新入力は成分の逆順で、各32 byteの正確なhalf格納である。両APIとも公開320-byte Storageの
offset256からの64 byteが独立に指定したFP32期待bit列と全byte一致した。
NaN/subnormal/負のゼロの追加契約は今回要求しない。境界外clamp、nearest対linearの違い、
画像の行/成分、逆順binding、型/owner拒否、未投入cancel、pending/busy、実完了後reuse、
runtime facade先drop、最終View/Image/sampler/heap/Device解放を確認した。
D32は旧版0.25/新版0.75を宣言・clearし、既存64-byte出力の4 texelのRを
`0x3e800000`/`0x3f400000`へ照合した。GBAの値は今回の受入保証に含めない。
旧版の再sampleは再clearせず旧値を維持する。attachment Viewの用途違い/別Image拒否、
clear取消後の未初期化read拒否と再clear、pending中の所有、最終解放を共通fixtureで確認した。
strictはVulkan Core/SyncValまたはD3D12 Debug Layer/GBVの実効を確認し、
最終native解放後までwarning/error/corruption/callback問題ゼロだった。
製品の追加検証/専用診断除外・native Requested Disabledでも同じbyte一致を別processで確認した。
この成功だけから外部Layer不存在やnative診断有効を主張しない。

今回の統合結果では既存StorageとRGBA8/RGBA16F/D32を含め、strict/製品の各Vulkan 5件、D3D12 2件、計14件が成功した。
D3D12のstorage 1件はuniform単独とStorageRead chainの両case、sampled 1件は3形式を実行する。
workerではGPUを実行せず、rootが統合結果を逐次実行した。正式ログは`.tools/b1-storage-read/gates/`に保存した。
変更範囲外のGraph/WSI全体の実GPU回帰は今回再実行していない。直近の全60件成功はGit履歴の`.2.1.4`の記録を参照する。

Linux実GPU、別GPU/driver、一般shader/owned Graph画像実行、comparison sampler/PCF、
性能・長時間耐久、実native OOM/Device lossは未実行である。性能優位や計算量の改善は主張しない。
既知の未再現起動crash `renderer-0pb`の原因確定や解決も主張しない。

<a id="repository-review"></a>

## ソース全体のレビューと効率修正

`renderer-420`、比較元`f649bcf`。第一者のRenderer Rust 64ファイル、example 11、
integration test 4、Bistro Python 28、validator 2、PowerShell 6と、benchmark、
独立試作、生成・依存設定、CI、文書構成を分担レビューし、主要な修正を別担当が再レビューした。
生成bindingsとshader成果物はFFI・ABI・所有・固定入力による再生成の境界で確認した。
配布asset全体の任意の破損や全driverの動作を証明するレビューではない。

| 課題 | 確認した問題と修正 |
|---|---|
| `renderer-420.1` | compile解析用の名前・初期範囲・依存の複製を借用へ変更。drawの中間配列を除去し、不変snapshotのパラメーター保証をprivateな値で引き継ぐ |
| `.2` | mesh uploadの完了prefix・残byte・全resident退役走査をcursor、残量、retiring索引へ変更。不要なLRU sortと重複Arc cloneを除去。readbackは退役後に一度sortして検索。importはowner/generation/revisionを維持し、深い不変metadata再比較だけ開発診断へ移す |
| `.3` | resize時とsampleの重複Fence pollを除去。再描画の期限を固定し、ゼロ寸法で待機する。非ゼロ寸法への復帰はSwapchain作成済みかどうかに依存しない |
| `.4` | 同一file/archive/FBXの重複hash、画像sourceの全量保持、Node Bufferの複製、出力されないtangent計算を除去。validatorによる入力上書きと、失敗した比較結果をsummaryが成功として公開する不具合も修正 |
| `.5` | 不変Device限界値を初期化時から再利用。参照transferの二重検査・初期化範囲の複製/書込ごとの再反映・記録後のplan保持を除去。広域dead-code抑止を撤去し、参照専用経路とprivate INFO履歴をtest構成へ限定 |
| `.6` | D3D12 readbackの全量zero-fill後の上書きを除去。結果検索/残数判定の二乗走査をsort・二分探索・残数へ変更し、保証されたCOM親interfaceへのQueryInterfaceを除去 |
| `.7` | 同一queueで既に可視化済みの完全に同じread scopeだけ、重複barrierを省略。CPU回帰は8読取の依存8件から1件。新stage/access、write、WAR、layout変更への依存は保持 |
| `.8` | PPMのpixelごとの小書込をbuffer化。計測reportにcompile featureを記録し、観測したnative検証のbenchmark混入を拒否。shader hashをstream化、照合textをLF固定、全SPIR-Vのcompile/検証成功後に公開 |

[機械可読の証拠と同一CPU harness](source-review.json)を保存した。Rust 1.99.0、release、
default featureなしで、2,048資源/4,096passのcompileはallocation要求46,146→40,001回、
総要求byte 7,230,276→7,125,062。8,192drawのframe構築は19→18回、
2,689,489→1,706,449 byteだった。後者はsnapshot作成後の測定で、常駐量・peakではない。
GPU速度、wall-clock、Bistro性能の改善率は測定していない。

Blender 4.5.16 LTSの同一入力でtangent計算の有無を比較し、1,296mesh/
2,832,120triangle、11,664配列比較とmetadata、全407個のglTF関連fileが一致した。
alpha再検証の出力39,662 byteも変更前後で一致した。
shaderは最後のcompileを故障させても全5個の既存SPIR-Vを保持し、
`core.autocrlf=true`のcheckout相当で13個のtextがLFとなって再生成照合に成功した。

構成上のGraphとengineの相互依存、Program/bindingと記録、物理資源とGraph、
WSIの複数所有境界の同居は、下記`renderer-hyf`で機能追加前に整理した。
[現在の境界](../design/implementation-policy.md#source-boundaries)を既存B1課題へ反映する。backendのprivate分離、生成bindingsの隔離、
`foo.rs`と`foo/`、integration test/実GPUignoredの区分は維持する。
現在のraw Graph作成境界のextent/clear検査、動的なowner/revision、同期・寿命・容量・native失敗は保持する。
将来の一般Programではread scope省略の成立条件を再確認する。
既知の未再現native起動crash `renderer-0pb`は未解決で、このレビューで解決したとは扱わない。

統合したRustソースでfmt、製品/開発の全target Clippy（`-D warnings`）、CPU通常66件、
`presentation,d3d12`のrelease通常180件、追加検証/Graph診断ありのrelease通常191件が成功した。
RTX 3060で逐次実行したreleaseの厳格Vulkan 46件、製品経路/新規回帰4件、
Renderer追加scanなしのbatch 1件、D3D12寿命1件、二API共有bufferの厳格/製品各1件の
合計54件が成功し、表示smokeも厳格/製品の両方で成功した。
各厳格gateは正常終了とnative最終解放後まで既存の診断判定を維持し、新規native警告/エラーはない。
製品構成の成功をnative検証が実効有効だった証拠へ流用しない。
Python 60件、Node 10件とshader再生成照合が成功した。
公式offline glTF validatorの0 error/1,591 warning/1,591 infoは従来の`issues`全体と一致し、
nativeの診断ゼロと区別する。Linuxは実GPU未実行で、CIのCPU/build確認を別に記録する。

<a id="source-reorganization"></a>

### B1実装前の4つの責務境界の整理

`renderer-hyf`、整理前`9f6b3a4`。`renderer-7ft.2.1`の一般Program機能を始める前に、
4点を実装・統合した。[設計](../design/implementation-policy.md#source-boundaries)と
[機械可読の検証記録](source-review.json)の`source_reorganization`に対応する。

| 対象 | 整理後の境界 |
|---|---|
| GPU非依存Graph | 公開facade、resource/pass/error/builder、共通解析を分離。Graphからengine型を参照せず、mesh変換はengine bridge、保証と追加はGraph builderが担当 |
| Vulkan Program | publication、creation、inline BindingPool、native recordを分離。作成・bindingにGraph依存なし。追加allocation/Arc/lockなし、lease保持とpool→Programの解放順を維持 |
| 両APIの物理資源 | backend直下のresources、Vulkan errorへ抽出。物理owner/factoryにGraph型の依存なし。D3D12のCPUアクセスは完了・範囲・排他的所有のprivate unsafe契約を明記 |
| Windows表示 | native Surface/Swapchain/slot/acquire guard/Program/commandsとGraph adapterを分離。native側にGraph状態を持たず、private slot/guardの操作が取消と退役を所有 |

公開入口はfacadeからの直接reexportで維持し、並行API、一般RHI、`mod.rs`を追加していない。
独立レビューでmesh provenance/Arc共有/失敗時の未追加、部分作成cleanup、
descriptorと参照資源、GPU＋present両完了、未確定失敗時の保持・旧世代退役を確認した。
依存方向の機械検査もすべて成功した。shaderのsource/binaryと、実行可能な固定操作の範囲は変えていない。

統合後のfmt、製品/開発の全target Clippy（`-D warnings`）、CPU通常67件、
`presentation,d3d12`のrelease通常183件、追加検証/Graph診断ありのrelease通常194件が成功した。
前節と同じrelease実GPU54件および製品/厳格の表示smokeが再び成功し、
resize/cancel/pending終了と最終native解放の既存診断判定を維持した。新規native警告/エラーはない。
Programのnative call site 38件とshader include 6件は不変。
WSIは53→52件で、唯一の差は不変limitsの再照会を既存Device cacheへ置換したことだった。
固定SDKのSPIR-V/DXIL再生成照合も成功した。

同じrelease CPU harnessで、compileのallocation 40,001回/7,125,062要求byteと、
frame構築18回/1,706,449要求byteが整理前後で一致した。
これは合成入力のallocation要求の確認で、実時間・GPU速度・Bistro性能の測定ではない。
Linux実GPU、別GPU/driver、Bistro全体、既知`renderer-0pb`の再現・解決は今回未実行である。

## Bistro計画と現行実装の区別

開発計画を[実装方針のB0〜B5](../design/implementation-policy.md#bistro-roadmap)へ改訂した。
追跡は`renderer-7ft`。`renderer-7ft.1.1`では利用者要件を合意し、
[確定要件と素材検証](../design/implementation-policy.md#bistro-requirements)を記録した。
対象は屋外全体、太陽光＋IBL＋影、PBR/HDR/tone mappingと利用側Bloom、基準1920×1080、
自由カメラと固定視点・自動巡回、両APIのWindows表示・操作、初回受入は既存RTX 3060である。
要件の合意を実装・実GPU受入の成功とは扱わない。
B0で維持・改修・置換・外部基盤を比較し、[native上位置換＋低レベル再利用](../design/implementation-policy.md#foundation-selection)を採用した。
旧M4の6/22完了は準備・Device・buffer実行の実績であり、新計画の進捗率へ流用しない。
旧M4の未着手分は後継課題を示して置換し、実装完了とは扱わない。

素材の取得・棚卸し・変換とCPU検証を下記へ記録した。`renderer-7ft.1.1.1`がこの作業の追跡先である。
全ゼロalphaに加え、最終camera/path/light/IBL/露出と再現・比較手順を下記のとおり固定した。
要求・比較入力の固定に続き、公開APIの利用例と基盤比較・採用・移行範囲を設計し、B0の設計を完了した。
B0ではRenderer本体・管理下shader・native経路は変更せず、以降の既存GPU・Rust試験表は従来の固定ソースの実績として保持する。
BistroのRenderer描画、独自Bloom、公開パスAPIの二API実証とD3D12表示は未実行である。
画像閾値の校正・性能予算・RTX 3060での全体収容も未実測であり、offline変換をその成功へ流用しない。

<a id="public-api-comparison"></a>

### 公開APIのUXと拡張契約の比較

`renderer-7ft.1.2`の[比較と第一案](../design/implementation-policy.md#public-api-comparison)を記録した。
構築時owned命令案Aとframe内借用callback案Bに同じ型付きpass dataを使い、記録時期と依存宣言の軸を分けた。
第一案はA＋binding／attachment由来のアクセスである。理由は実命令の確定境界と所有契約の単純さで、
CPU／GPU速度の優位を実測した判断ではない。基盤選定の`.1.3`でもAを維持した。B1の実Renderer二API実証は未完了である。

[Rust試作](../../crates/renderer/examples/graph_api_comparison.rs)はGPU非依存のCPU意図を比較する。
複数draw、depth-only／color-only、独自Compute、module内の多段Bloom、履歴初回／継続／resize、
将来のMRT／mip Viewを同じ入力で書き、ownerだけを正規化した命令とアクセス用途を照合する。
短命なdraw slice終了後のAの命令保持、Bのframe借用と不要callback非実行、
owner／範囲／bindingの不一致と宣言外アクセスの拒否を対象にする。
再現は[開発ガイド](../guides/development.md#public-api-comparison)を参照する。

Windows x64／Rust 1.99.0でworkspaceの既存CPU試験65件と新規9件が成功し、
新規9件はdebug／release双方で確認した。fmt、CPU全target Clippy（`-D warnings`）、release比較実行も成功した。
独立レビューの指摘を反映し、Loadのread用途、Bloomの抽出／blur／無効化、重複writeとcallbackの出力欠落を補った。

| 比較入力 | probeが保持したpass | CPU命令 | 資源用途 | Aのbuild／record callback | Bのbuild／record callback |
|---|---:|---:|---:|---:|---:|
| 履歴初回 | 12 | 22 | 24 | 13／0 | 0／12 |
| 同extentの履歴継続 | 12 | 22 | 25 | 13／0 | 0／12 |
| extent変更によるreset | 12 | 22 | 24 | 13／0 | 0／12 |

各入力で13passを登録し、未使用effectを1pass除去した。両案の命令・用途は各入力で一致した。
Aは除去前のbuild時にenum本文1,840 byteを構築し、Bはその時点で命令を構築しない。
Bにもpass dataとcaptureの保持があり、この値だけで総メモリや速度を比較しない。
文書のUTF-8／fenceと相対link／anchor、`git diff --check`を確認した。

試作の選択処理は画像単位の保守的な後方到達性probeであり、現Rendererのcompilerを実装し直すものではない。
範囲の局所検査と利用コードの一致を確認するが、mip単位のproducer／culling、初期化、
完全なRAW／WAR／WAW、native barrierを検証しない。mesh／programの識別、history import／exportも記号的表現で、
native参照の強い保持、canonicalな予定状態、revision、submit失敗・GPU完了の実証ではない。
件数・enum本文byteは構造的な観測であり、nested Vec、callback capture、allocator、catalog等を含む全保持量ではない。
shader生成・ABI・GPU内容・Windows表示・実GPU性能は今回未実行で、Renderer本体とshaderの実装を変更していない。
Vulkan指針の用途由来同期・記録から完了までの寿命と検証方針を設計へ適用し、API契約からの例外はない。

<a id="foundation-comparison"></a>

### 基盤の比較・採用と小試作

`renderer-7ft.1.3`の[採用理由・代案・工数・移行範囲](../design/implementation-policy.md#foundation-selection)を記録した。
固定操作の維持は独自パス要件を満たさず、全面native置換、safe wgpu、wgpu-hal直接利用と比較した。
採用はnativeの低レベル所有・同期・退役・WSIの再利用と、上位の一般owned命令／Program/bindingへの置換である。
safe wgpuのnative実装負担の小ささとRenderBundleによる事前検査を認めたうえで、
現AGENTSの保証済み内容の反復検査を製品経路から外す規約を維持する判断とした。
nativeの速度優位や工数実績を証明したものではない。B1〜B5に受入条件・依存を持つ17子課題を作成した。

[独立試作](../../tools/foundation-probe/src/main.rs)はwgpu 30.0.2を固定し、Rendererのruntimeへ接続しない。
短命draw Vecを即時closureからowned planへ移し、depth-only／color-only各2draw、
利用側WGSL Computeでcolor/depthを読む3passを8×4で記録した。
submit直後にplanと呼出側textureをDropし、実submissionとmap callbackの完了を待って128byteを照合した。
source／lockfile／shader／実行ファイルのSHA-256、原byte、adapter情報と4runを
[共有JSON](foundation-comparison.json)へ保存した。再現は[開発手順](../guides/development.md#foundation-comparison)を参照する。

| 実行要求（release、二backend逐次） | Vulkan | D3D12 |
|---|---|---|
| native flags空（bits 0） | 全128byte期待値一致 | 同左、Vulkanとも全byte一致 |
| DEBUG＋VALIDATION要求（bits 3） | 全128byte期待値一致 | 同左、Vulkanとも全byte一致 |
| pipelineなしdrawの負の対照 | 両flagsでcore Validation errorを返した。無効命令は未submit | 同左 |

Windows x64、Rust 1.99.0、NVIDIA GeForce RTX 3060、vendor 4318/device 9351、
Vulkan driver 610.88／D3D12 driver 32.0.16.1088を観測した。
vendor/device/nameの一致を確認したが、この試作ではLUIDを照合せず、同型複数adapterの同一性を証明しない。
有効fixtureのwgpu API Validation errorはゼロ。負の対照の期待errorと区別する。
depthの読取りが結果へ反映されるよう、出力blueは描画depth 0.25のUNORM値64を期待する。
depth drawを空にした一時変異ではclear値1.0からblue255となり、内容不一致で失敗することを確認して復元した。
native検証の実効、SyncVal/GBV、最終native解放後のwarning/errorは未観測でJSONのnullとし、
**この小試作を既存の厳格native受入の合格として数えない。**

全`Study::scenario`／多段Bloom、MRT/mip、履歴revision/reset、Windows表示・自由camera・resize/最小化・
pending終了、native失敗注入、HAL単独の安全性、全Bistro実行と性能・メモリは今回未実証である。
全代表入力の比較表はAPI写像の設計判断であり、この3passだけで同期・寿命の全契約を証明しない。
wgpuは独立workspaceの採用実験に限定し、Rendererの依存とnative実装を変更していない。
Vulkan指針の能力照会・用途由来同期・所有と実完了、検証方針の保証境界を選定へ適用した。
API契約・一般推奨・既存GPU受入からの新しい例外、vendor固有最適化、性能主張はない。

統合時にworkspaceのCPU試験65件、API比較9件のdebug/releaseと比較実行、fmt、CPU全target Clippyを確認した。
独立試作のfmt/check/全target Clippy/通常test（実GPU1件はignore）、release buildと上記4実行が成功した。
独立依存のcargo-deny 0.20.2はadvisories/bans/licenses/sourcesが成功。
hashbrown 0.16/0.17とsyn 2/3の重複warningを確認し、advisory除外は追加していない。
foldhash 0.2.0のZlibは独立試作の監査設定だけで個別許可し、Rendererの依存方針を拡張していない。

<a id="bistro-assets"></a>

### Bistro屋外素材の実体検証

入力・tool/function hash、全画像の対応、材質recipe、全meshの誤差とValidator原文は
[機械可読の現在データ](bistro-assets.json)、見た目は[比較画像](bistro-assets-preview.png)、
再現は[開発ガイド](../guides/development.md#bistro-preparation)を参照する。

原本ZIP・検証済み屋外glTF一式・環境HDRと上記比較PNGはGit LFSで共有する。
[素材一覧とライセンス](../../assets/bistro/README.md)、[全412ファイルのmanifest](../../assets/bistro/manifest.json)で
パス・byte数・SHA-256を固定した。固定条件PNGを含む実体は計1,775,343,744 byteで、既存の検証済み407ファイルもbyte一致する。
重複画像を共有するLFSオブジェクトは210個、計1,601,410,069 byteである。
原本・変換済み素材・従来の比較PNGは今回もbyteを変更していない。
ライセンス・検証JSON・manifestは通常Git、toolや作業中間物は引き続きGit管理外である。
既存410ファイルのLFS共有時には、originへ208/208オブジェクトの送信が成功し、ローカルキャッシュを共有しないdetached checkoutで
全410ポインタと空キャッシュを確認した後、originから全量取得して全ファイルのsize/SHA-256一致を確認した。
元checkoutと取得先の`git lfs fsck --objects --pointers`も成功した。
共有hooksは全6本を実行可能として追跡し、pre-pushのstdin保持・LFS失敗時停止と
`bd hooks install --beads`による全hookのbyte保持を確認した。
manifest・ポインタ・素材の変更時は取得境界で再照合する。Rendererのdraw/submitへ全量走査を追加していない。

| 確認 | 結果 |
|---|---|
| 公式入力 | `Bistro_v5_2.zip` 894,377,473 byte、SHA-256 `0d50e3c724c6c5da19f8eb99ad3f53e36fec37ffa2df9621f9ccf0603f3934e1`。FBXは`471991629c7e35a88338c48f3565390ad19cc5d1d7c7e067fc2a60b4f3b5f195`。同梱README/licenseもhash保存 |
| 独立FBX解析 | 1,296 Geometry/mesh Model、2,832,120三角形、132材質・405画像、欠落参照/名前衝突なし。有限値、index/normal/UV/material slot範囲と接続を確認 |
| 元FBXから取込結果への対応 | 全1,296 Model/Geometryの元float32位置・向き・raw corner normal・両UV・面ごとの材質slotが完全一致。428件の長い名前の短縮も追跡し、衝突なし。world行列の元FBXからの再導出はこの検査に含めず、取込後からglTFへの行列保持を別に確認 |
| 元の面と属性 | normal＋TextureUV/LightMapUV、tangent/colorなし。重複面2,894（逆向き980）、厳密な面積ゼロ19を保持。重複面のうち2,609は法線、765はUVが異なる |
| 変換・材質 | Blender 4.5.16 LTS build 3f6797307da5。通常の面削除とnormal精度低下を避ける限定profileを使用。132材質のfactor/alpha/両面/UV/チャンネル、405画像の変換画素を照合 |
| 独立glTF形状比較 | 全instance・三角形・向き・重複・材質・raw source normal・UV0/UV1を照合して成功。最大world位置差2.60e-5 m、normal成分差3.90e-7、UV差0。float32の絶対/相対許容値はデータに記録 |
| 形式検査 | Khronos Validator 2.0.0-dev.3.10、外部buffer/画像も検査、エラー0。TANGENT consumer生成の警告1,591、保持した未使用LightMapUVのinfo 1,591を抑制せず保存 |
| 再生成 | 元FBXを別Blender processで再取込し、glTF・buffer・全405画像、計407 file / 851,291,697 byteがすべてbyte一致 |
| CPU画像QA | 元FBXから明示PBR recipeを構成したsceneとglTF再importを同じ3視点・固定光・Cycles CPU 8 sample・960×540で比較。形状/材質の大きな欠落は見られず、RGB PSNR 34.8〜39.2 dB。完全一致やruntimeの合否閾値とは扱わない |

raw normalの圧縮表現を使うとBlender内ではゼロ/方向差が生じたため、glTFデータは保存したraw値を直接正規化して出力した。
CPU previewにはBlenderの再符号化・sample noise等の差が残り、屋根等の局所差も画像に残す。
glTF再import後のBlender終了ログには24 byteの未解放block報告が1件あり、tool診断として保持する。
Rendererのnative所有・GPU完了の受入を行った結果ではない。

**確定した素材解釈（`renderer-7ft.1.1.2`）:** `MASTER_Glass_Dirty_MASKED`は68面/13mesh、
`Paris_LiquorBottle_01_Labels`は154面/11meshで使われる。元alphaを保持し、不透明overrideは加えない。
両材質の面はopacity 0によって不可視で、shadowにも被覆を作らないことを後続実装の受入条件とする。
いずれも非退化面であり、mesh/materialを削除する決定ではない。
原本・glTF・全画像はbyteを変更せず、glassはMASK/cutoff 0.5、labelsはBLENDを維持する。
mask/blendの分類と、ORMのないラベル材質のnonmetal/roughness 0.5も明示した変換方針である。

元DDSのDXT5 alphaを独立に全mip復号し、glassの12段・labelsの5段で全sample 0を確認した。
既存PNGへのbase mip変換もRGBA一致し、labelsはRGBも全0である。不透明化でラベル画は復元されない。
同梱READMEの不透明度規約・屋外pysceneのoverride不在と、Falcor 4.0.1のBC3読込・alpha付きtextureのmask分類・
texture alphaによる除外を照合した。4.0-previewと4.4でも整合を確認したが、配布物はexact Falcor commitを指定していない。
現行labelsのBLENDとFalcorのMASKを一般に等価とは扱わず、固定したalpha 0での被覆0が整合するという判断である。
[確定方針と代替案](../design/implementation-policy.md#bistro-alpha)、[入力・一次ソース・計測](bistro-alpha.json)、
[alpha保持／不透明化／差分／対象coverageの比較画像](bistro-alpha-preview.png)を保存する。
比較は全sceneのmesh・材質を保持し、対象材質だけを変更するCycles CPU診断である。
1,296 mesh・2,832,120面を保持し、32 sample・seed 0・960×540、固定camera/lightで6画像を生成した。
瓶の近景には全条件共通の固定area lightを追加し、暗所での見落としを避けた。
不透明copyの対象coverageは窓97,308／瓶164,497画素で、その領域内のRGB差>2は59,855／126,290画素だった。
値は低sampleのnoise・間接光変化も含む診断結果であり、runtimeの合否閾値へ使わない。
このalpha調査で作者の意図・Falcor実行・Renderer実GPU画像/性能・最終camera/lightの受入は確認していない。
構図・照明入力の選定は下記の別CPUプレビューで確認し、実GPU受入は後続に残す。
取込/cook境界の保証と変更時の再検証に限定し、draw/submitへ検査や診断payloadを追加していない。
alpha追加分を含むPython 41件と既存Node 5件が成功した。Nodeのsymlink試験はsandbox内のEPERM後、
通常権限で5件すべてを再実行して成功した。Rust/native実装変更がないためローカル実GPU試験は今回対象外である。
文書5件のUTF-8/fence・相対link/anchor 107件、全411 payloadのsize/SHA-256、
alpha判断を含む素材reportの再生成と`git diff --check`も成功した。独立レビューの指摘を反映し、
glTFで省略されたMASK cutoffの実効既定値0.5を判断データへ記録した。

元にないTANGENTはこのglTFへ生成収録していない。標準生成を試した別条件ではzero tangent 345個が形式エラーとなったため、
MikkTSpace生成と退化入力fallback・実行用mipをB2の取込/cook要件へ記録した。
PNGはbase mipだけで、元DDSとmip情報は保持する。これらの警告はnative API検証ゼロ方針の例外ではない。
素材はAmazon Lumberyard Bistro / NVIDIA ORCA / [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/)。
配布元は[公式ページ](https://developer.nvidia.com/orca/amazon-lumberyard-bistro)であり、画像は本検証で変換・描画した。

検証ツールのローカルCPU試験はPython 34件・Node 5件成功。独立レビューで材質検査のAO用UV/transformと
予期しない発光の検出、重複三角形の対応、元FBXから正規化normalへの照合を補強し、回帰試験を追加した。
文書4件のUTF-8/fence・相対link/anchor 79件・既存policyへのbacklinkと`git diff --check`を確認した。
LFS共有追加では文書7件のUTF-8/fence・相対link/anchor 106件も確認した。
通常CIへ小fixtureの`asset-tools`を追加した。ローカルのRust/native GPU再実行はRenderer実装変更がないため対象外とし、
pushした組合せのbuild/testはCIで別に確認する。

<a id="bistro-conditions"></a>

### Bistroの描画・比較条件

`renderer-7ft.1.1`の固定入力は[conditions.json](../../tools/bistro/conditions.json)、
判断と再現条件は[実装方針](../design/implementation-policy.md#bistro-conditions)、
手順は[開発ガイド](../guides/development.md#bistro-conditions)にまとめた。
[機械可読の実行記録](bistro-conditions.json)と[CPUプレビュー](bistro-conditions-preview.png)に、
入力/設定/tool/画像のhash、全scene保持、照明候補と未実行の範囲を記録する。

固定7視点（全景・路地・材質・glass・label・巡回中間・折返し）と、
30秒/60 Hz/1800 frameの往復巡回を指定した。基準は1920×1080、near 0.02/far 500 m、
右手Y-up/m・縦FOV・論理上原点の画像・通常Zの0〜1とし、native変換は同じrayを保つ。
cameraの自動露出・jitter・DOF・motion blurは使わない。設定の有限値・参照・視線の補間特異点、
閉路と半開frame列、入力4件のsize/SHA、HDR方向のcardinal/逆yawをCPUで検証した。

同梱HDRの原本を保持し、元texelのRec.709輝度を4へcapして高輝度源を抑え、別太陽を加える。
輝度100超の213 texelが全周の立体角加重輝度積分の44.902%を占め、cap 4で変更するtexelは68,105件だった。
この処理は太陽の完全な分離でも照度の物理校正でもない。未処理IBL＋太陽、IBLのみとの比較を保存する。
cameraに見える背景は一定の線形RGB .18とし、環境内の別建築をBistroのgeometryに見せない。
IBLは照明・反射へ残す。固定露出EV +1、commit固定のKhronos PBR NeutralとsRGB一回で出力する。

CPU画像はBlender 4.5.16 LTS build `3f6797307da5`、Cycles CPU・960×540・32 sample・seed 0、
adaptiveなし、限定したbounce数、CPU OpenImageDenoiseの表示用画像で構図・露出を判断した。
全1,296 mesh・132材質・2,832,120三角形を保持し、元glTF/buffer/textureのbyteは変更していない。
全景は全mesh境界を各辺5%以上の余白でfrustum内へ収めた。巡回のeye経路4 segmentと
全1800 frame×5本のnear-plane rayで侵入0を確認するが、連続したfrustum体積や歩行可能性の証明ではない。
noise除去前/後のscene-linear 32-bit EXRとPNGを生成し、CSM/PBR/IBL前処理/Bloom/AAの実GPU正解画像とは扱わない。
太陽は仰角30度/RGB 1、IBLはcap 4/強度3、露出はEV +1とした。
瓶の近景はfillを追加しておらず暗いため、alpha被覆を色画像だけで受け入れない。

画像閾値は同API反復3回以上・二API差・既知の誤りへの検出感度で受入前に校正する手順とし、
性能はcold準備、1800 frame×2 warm-up＋×3計測、生sampleとphase別p50/p95を記録する手順を固定した。
数値閾値・FPS/VRAM予算は未校正で、B2/B3/B4/B5の実装・RTX 3060実GPU受入に残る。
alpha 0のcolor/depth/shadow被覆0、二APIの表示/操作/resize/終了と破棄後の厳格native検証も未実行である。
near .02/far 500 mの通常Zにおける遠方のdepth精度も未検証であり、B1/B3の実GPUで確認し、必要なら条件を改訂する。
Python 57件・Node 5件、文書5件のUTF-8/fence・相対link/anchor 124件、全412 payloadのsize/SHA、
固定条件を含む素材reportの再生成とdiff checkを確認した。改行変換による記録hashの不一致を防ぐため、
条件JSONの元byteと新規tool/検証JSONのLFを属性へ固定し、既存alpha reportの参照SHAもGit blobに一致させた。
Rust/native変更がないため、ローカル実GPU試験は今回対象外である。
Rust/native/shader・製品版の検証経路は変更せず、入力走査をdraw/submitへ追加していない。

## 環境と対応範囲

| 項目 | 確認値 |
|---|---|
| OS / target | Windows x64 / `x86_64-pc-windows-msvc` |
| CPU | Intel Core i5-14600KF |
| Rust / Cargo | 1.99.0 / 1.99.0 |
| GPU / driver | NVIDIA GeForce RTX 3060 / NVIDIA 610.88 |
| Vulkan SDK / Loader | 1.4.341.1 / 1.4.341 |
| Instance / VMA要求API | Vulkan 1.3 |
| Queue | family 0、index 0、Graphics＋Compute、表示時は同じqueueのpresent対応も照会 |
| bindings / allocator | ash 0.38.0+1.3.281、vk-mem 0.5.0、同梱VMA 3.3.0 |
| Windows表示依存 | winit 0.30.13、ash-window 0.13.0、raw-window-handle 0.6.2 |
| D3D12 | Windows限定、private bindings 0.100.0、単一DIRECT queue、committed buffer、legacy barrier |

CPUのGraph・engine入力はGPU・windowなしで構築できる。
表示はWindows、EXT swapchain maintenance1と依存拡張・feature、
同一Graphics＋Compute＋present queue、SRGB_NONLINEARのRGBA8/BGRA8_SRGB、OPAQUE対応に限定する。
実機で必要能力を照会した。固定bindingsにあるEXT版を利用し、headlessの必要能力は増やしていない。
別queue、拡張なしfallback、透明Window、HDR、任意shaderは対応したと扱わない。

## 品質ゲート

| 確認 | 結果 |
|---|---|
| fmt | 成功 |
| CPU / Vulkan / Windows presentation / D3D12 / Vulkan＋D3D12の全target check・Clippy `-D warnings` | 各backendの製品／開発＋dump、計10構成成功 |
| CPUのみの通常試験 | 両構成とも単体53件＋公開API 9件＋負荷fixture 3件成功 |
| Vulkan featureの通常試験 | 製品145／開発157単体＋公開API 9件＋負荷fixture 4件成功、45／44 ignored |
| presentation featureの通常試験 | 製品150／開発162単体＋公開API 9件＋負荷fixture 4件成功、46／45 ignored |
| D3D12 featureの通常試験 | 製品63／開発62単体＋公開API 9件＋負荷fixture 3件成功、2／3 ignored |
| Vulkan＋D3D12 featureの通常試験 | 製品155／開発166単体＋公開API 9件＋負荷fixture 4件成功、各48 ignored（公開二API受入と負荷fixtureを含む） |
| 最適化・独立feature | releaseのVulkan／D3D12／併用、各製品・開発構成のCPU試験成功。各backendのnative診断だけ／Graph診断だけ／開発検証・dumpなしのcheck、受入workerの併用5構成Clippy成功 |
| native検証ありの明示GPU試験 | release、実Win32 Surfaceを含むVulkan 43件を再実行して成功、逐次実行 |
| 製品経路の明示GPU試験 | release・診断featureなし・native Disabledのmesh画素照合1件成功。CPU upload Arc解放・provenance保持0・index走査0も確認 |
| D3D12 runtime | releaseの厳格検証／製品構成各1件成功。共有所有、設定不一致拒否、終了と同一process内再作成を確認 |
| D3D12 buffer寿命 | release厳格検証1件成功。未投入キャンセル、作成途中の注入失敗、逆順待機、runtime先行終了、pending Drop、空Graphの最終解放順を確認 |
| 二API共通buffer Graph | clean `2b15cd0`でrelease厳格検証3回＋製品3回成功。同じ10-pass Graph・同じGPUで各308 byteが全一致 |
| D3D12準備の実機回帰 | 既存3回の準備受入に加え、clean `2b15cd0`で反復scriptを1回再実行して成功。固定shader再生成・同一GPU・厳格診断を確認 |
| Validation / SyncVal | 実効設定を確認し、native所有者の破棄後までwarning・error・callback失敗ゼロ |
| Shader再生成検証 | 固定SDKの5種類のSPIR-V検証・再生成一致に加え、HLSL・検証済みDXIL・reflection・manifestの再生成一致 |
| headless例／frame loop例 | 既存のPPM・24-frame照合結果を継承。今回CLI本体の再実行はなく、同じ操作・寿命はGPU試験と下記負荷でも確認 |
| engine負荷 | releaseの開発検証あり45画像／製品519画像が全RGBA一致。大mesh、47 chunk upload、更新・削除、複数View、3 in-flightを確認 |
| engine Windows表示例 | releaseの開発検証ありでVSync ON/OFF各起動、製品構成でも`windowed --smoke`成功。各12 Present要求、VSync往復・resize・最小化復帰・pending終了 |
| 表示例の引数検査 | VSync既定ON、ON/OFF指定、smoke併用、不正引数拒否のCPU試験1件成功 |
| exampleのCPU試験 | frame計測9件、大メッシュfixture・統計4件、windowed引数1件成功 |
| 依存監査 | 今回は依存追加・lockfile変更なし。cargo-denyの既存成功結果を継承し、CIでもworkspace・独立generatorを再確認する |
| Rustdoc | 製品／開発＋dump構成で`RUSTDOCFLAGS=-D warnings`成功 |
| 文書 | UTF-8、相対リンク・anchor、fenced block、差分空白を確認 |

実GPUはignored試験を明示実行し、前提不足を成功・skipへ置き換えていない。
CPU fallbackの成功で実GPU受入を代替しない。
今回のM4-03/04統合確認のソースは`2b15cd0bc7ca68892f433c1a67d5033278f7252f`、実行時のmainはcleanである。
品質確認の全コマンド・終了値は`.tools/m4/runtime-buffer/gates/summary.json`、
実GPUの逐次実行は同`gpu/summary.json`へ保存した。既存engine負荷・表示smokeの数値は各節の固定ソース結果を継承する。
入力拒否を確認するアプリケーションERRORログはValidationの異常とは区別する。
最終品質ゲートでは`CARGO_INCREMENTAL=0`を使い、初期の並行確認で見られた
hard-link不可というincremental cacheの環境warningをソース診断と区別した。

Windows / LinuxのCPU・Vulkan、およびWindows presentationのcheck・Clippy・CPU試験を
[GitHub Actions](https://github.com/tsukushibito/renderer/actions/workflows/ci.yml)に設定している。
このローカル確認はLinux実行や実GPU CIの合格を意味しない。

### 製品版と開発用の検証分離

ライブラリの最終確認点は`e96d270`、表示例の追補は`aa5f1eb`。
`renderer-y7e`／`fa3`／`585`／`vmj`／`kyt`／`jp2`を統合した。
profileと独立した`development-validation`、`vulkan-diagnostics`、`graph-diagnostics`を実装した。
製品の既定native modeはDisabledで、診断なしのRequired要求は明示的に拒否する。

Graph構築が保証したowner・範囲・用途の独立再検査とdead部分だけの循環検出を開発用へ移した。
producer・初期化・culling・live hazard／循環・最終内容・import revisionの保証は維持する。
管理下meshは生成時の不変count／最大値をprivate uploadへ引き継ぎ、一般writeで失効する。
位置を保つ完全copyだけが継承し、証明できない部分範囲は拒否する。
任意Graphの部分byte更新には入力保証としてprovenanceを残す。
同count・交互の頂点数の32 drawで、一般入力の走査は旧32回から1回、管理下meshは0回となるCPU回帰を確認した。

native診断offのmonitor／内部handleはZSTで、診断専用Arc・callback・履歴・debug_utils・命名／label構築を持たない。
Graph診断offはdump API・専用依存／寿命metadata・culled名・未使用transient名を除外する。
live／export／importの運用エラー用の名前は残す。通常のreleaseライブラリを使う外部allocation probeで、
1024資源の保持量を447,488→352,256 byte（全culled）、899,072→774,144 byte（2048 live pass）と確認した。
開発検証のon/offでは変わらず、Graph診断だけが保持を追加する。各caseでplan破棄後の収支は0。
これはallocatorへの要求byteで、overhead・stack・process RSS・VRAMではない。

release LLVM IRでもlowering dump固有の4文字列が製品構成に存在せず、Graph診断ありのpositive controlには存在した。
operational log用のtracingは残る。同期・完了・容量・native失敗を検証用cfgへ移していない。
最適化＋検証あり／なしのCPUと実GPUの両経路を確認した。

trim_idle_resourcesは明示的なworking set境界で、登録中・pending・escaped leaseを保護する。
実GPUでidle画像2個を解放し、VMA allocationは7→5件、9,888→8,864 byte、cache論理byteは320→0。
登録中64 byteとretained 132 byteは維持し、全owner退役後はstagingの2件／8,192 byteへ戻った。
VMA blockは100,663,296 byteのままなので、process VRAM削減やbyte budgetの保証とはしない。
世代上限・古いkey・失敗後の空きslot再利用も確認し、hidden waitを追加していない。

Rasterは名前付きdraw／typed attachmentとClear(value)へ移行し、未使用clear入力を除去した。
解析専用declare_passの競合拒否は安全な構築境界に残し、共有O(A log A)の検査へ変更した。
全read 8192宣言は線形・一時allocationなしで、元のbuilderの最初の衝突pair／error contextを保つ。
資源所有・準備・記録を既存private moduleへ分離し、独立レビューでも契約の欠落は見つからなかった。

native検証ありのrelease GPU43件はCore／SyncValの実効を確認し、native破棄後までwarning・error・callback失敗ゼロ。
別runの製品mesh試験は画素とCPU upload Arcの解放、保持segment・走査ゼロを確認した。
製品monitorは収集しないため、件数0や起動報告の不在を外部Layer非注入の証明にしない。

### VSyncと表示例

既定ONはFIFO、OFFは実Surface対応を確認したIMMEDIATEで、未対応要求はUnsupportedとなる。
同値変更、acquire／present、resize、最小化復帰、取消とpending終了を実Surface試験で再確認した。
表示例は開発構成のVSync ON/OFF各起動と製品構成の3回、releaseのsmokeが終了コード0。
開発構成はCore＋SyncValの実効と終了後の診断ゼロを要求し、製品構成はmonitor処理を持たない。
実際のVキー手動入力、別GPU／driver、driver／compositor設定、表示FPSやPresentMonは未確認である。

## 動作・寿命・異常経路

| 領域 | 確認した契約 |
|---|---|
| Graph解析 | byte範囲ごとのproducer、RAW/WAR/WAW、生存解析、循環、未初期化の穴、Attachment load/store/discard、dead宣言も含む検査、安定dump |
| native同期・内容 | upload/copy→vertex/index、color/depth→Compute→sampled composite→readback、端数dispatch、全byte/画素照合、用途別Programの保持、初回画像遷移の既存driver対策 |
| import/export | pending producerをCPU waitせずconsumerへ引継ぎ、owner/generation/内容revision、read-onlyと表示は内容revisionを維持、投入時の同期rebase、書き込み後の旧内容拒否、削除と再利用 |
| frame進行 | metadata Dropは非待機、Runtimeのbounded pending、投入前capacity拒否、単件poll／結果を返すwait、共有timeout、明示drain、native完了とreadbackのCPU寿命の分離 |
| 再利用 | 3 in-flight×4 roundで未読24 readback leaseを保持して再利用、warm-up後の作成数はresource 9 / execution 3 / descriptor arena 3で安定、結果byte不変 |
| 容量・削除 | pending中の削除と旧key拒否、別entry/世代、size切替、4096-byte readback容量の不足と結果消費後の回復、既存の未投入失敗回収 |
| history | 18 frameの連鎖＋read-only利用、初回・同extent reset・5×3へのresize、3件連続のhost未待機submit、pending consumer中reset、CPU参照との全byte照合 |
| engine入力 | owner/世代付きmesh/material CRUD、immutable snapshot、旧snapshotの独立利用、View/model/tintの画素変化、empty draw、実index上限、GPU pending中の更新・削除 |
| 準備と投入 | upload準備とbuildを分離、同一snapshotを独立rendererへ入力、2 View先行build、旧版と新版の独立常駐、foreign prepared Graphの拒否、pin中の退避保護 |
| custom pass | crate外のconsumerによる標準Graphへの処理追加、同寸linear RGBA8の契約、foreign handle/format/extent/未初期化の拒否、不要branchの除去 |
| 計測query | 通常off、pending lease保持、disable後の受理済みsample回収、完了後即再利用、readbackの遅延消費、有効bit幅・wrap・availability |
| WSI | 実Win32 Surfaceで18表示、各frameのheadless画素照合、resize/連続resize、ゼロextent/最小化復帰、取得後cancel、pending close、Windowの保持と解放 |
| 失敗状態 | CPU境界試験でsubmit OOMの状態不変、timeout/不確実失敗の保持、Device loss、presentのenqueue区分、SUBOPTIMAL、上限なし画像数等を確認 |

追加のupload境界試験では、Fence完了後の結果取得エラーだけなら退役済みentryを回収して次へ進み、
未完了Fenceエラー・timeoutでは所有を維持することを確認した。実driverへのquery OOM注入とは区別する。
presentの`OUT_OF_DATE`は`RecreateRequired`で、受理された`Presented`とは別にするCPU回帰を追加した。

未完了所有の保証は「GPUが遅いはず」という時間の推測に置かず、CPU状態試験と
実際の複数未待機submit・内容照合で分けて確認した。
WSI実試験は主に正常/resize/cancel経路であり、native OOM・Device loss・Surface lossを
実driverへ発生させた試験とは区別する。

frame/metrics、engine/custom、WSIを独立レビューした。
CPU retireにnative実行枠のreset費用が含まれない点、release失敗後の再release、
terminal pending残留による回収停止は修正し、最終差分を再確認した。
追加の確定した同期・早期解放問題は見つかっていない。これは未確認環境までの保証ではない。

push後のLinux CIで、Instance拡張名をpointerの値で整列する実装がOSごとに異なる順序を返し、
既存lifecycle試験が失敗した。C文字列の内容による整列・重複除去へ修正し、
別heapの同名文字列を使う回帰試験を追加した（`renderer-u4h.14`）。
修正後のVulkan／presentation全target check・Clippy・CPU試験と実GPU26件を再実行して成功した。

## 採用した基準と制限

既存ExecutionPool・staging・画像/View・descriptorを活用し、frameごとの新設poolや
全面idleを導入していない。通常frameはpollで回収し、Dropの待機に依存しない。
単一queue、物理aliasingなし、通常descriptor set、管理下shaderという基準を維持する。

表示はGraphと同じqueueへの別submitとし、Graph解析にSwapchain所有を持ち込まない。
sourceのcanonical状態とGPU leaseを共有する。submit追加のCPU費用・latencyを最適化済みとは主張しない。
EXT maintenance1のpresent fenceと通常GPU参照の両方を回収根拠に使い、描画Fenceだけでpresent終了を推定しない。
旧Swapchain作成失敗、enqueue済みpresent error、取得後cancelの状態を区別する。
キャンセル等の例外回収には10秒のAPI待機予算があり、通常frameの待機とは分ける。
完了やrelease結果を証明できなければ停止し、依存するnative所有連鎖を保持する。

ViewのABIはcolumn-major MVP 64 byte＋linear tint 16 byte。従来rasterはidentityで互換を維持する。
独自shaderの将来追加にはpipeline/binding metadata/ABI/信頼境界の検証が必要で、
M3のGraph構築拡張をその完了として扱わない。一般の公開パスAPIは未実装であり、新計画のB0で設計しB1で二API実行を検証する。

## 計測経路と基準値

以下の`752e785`は従来の128×96基準負荷の保存値で、現行APIの再計測値ではない。
現行schema3のCPU各phaseと待機の分離は開発ガイド、大メッシュの測定値は後述のエンジン負荷節を参照する。

`frame_loop --benchmark`にrelease・明示Validation設定・任意query・raw JSON保存を実装した。
対象は128×96、3 in-flight、標準描画・custom SwapRedBlue・1×1 history・毎frame readbackのheadless Graph。
warm-up 32 frame＋測定128 frameを3 run行う。表示/VSync/FPSの測定ではない。
CPU build/compile/prepare/record/submit/retire、GPU Graph区間、poll費用と観測latency、
論理byte、staging予約high-water、VMA Allocation/blockを分ける。
process VRAM/RAMやdriver/Swapchainの確保はこのVMA値に含めず、process budgetは未取得とする。

実装commit `752e7859cb55d50fd270dc7d74b67274d8c191a6`、worktree cleanで3 runを実行した。
後続の拡張名集約修正はcold初期化とCPU回帰試験だけを変更し、測定対象のframe経路・shaderは変更していない。
[計測JSON](current-metrics.json)に環境、要求/観測Validation、各runの統計、全384 sampleを保存する。
全sampleでGPU timestampは64 valid bits、1 ns/tick、availabilityを確認できた。
次の範囲は各runの中央値とnearest-rank p95の最小〜最大で、単位はµs。

| 区間 | runごとの中央値の範囲 | runごとのp95の範囲 |
|---|---:|---:|
| CPU build | 1.30–1.60 | 1.50–2.00 |
| CPU compile | 9.40–10.90 | 10.10–12.40 |
| CPU prepare | 2.40–2.90 | 2.60–3.80 |
| CPU record | 8.40–10.35 | 8.90–15.90 |
| CPU submit | 7.40–8.00 | 8.60–10.00 |
| CPU retire | 1.00–1.10 | 1.10–1.40 |
| 完了を返したpoll全体 | 1.50–1.70 | 1.70–2.10 |
| submit returnから完了観測 | 147.35–187.65 | 153.70–259.20 |
| GPU Graph区間 | 22.080–22.304 | 22.496–22.752 |

phase別中央値の和をframe全体の中央値として扱わない。観測latencyはpoll頻度と他frameのCPU作業にも依存する。
run 1にはCPU compile最大89.9 µs、record最大83.8 µsの外れ値があり、全sampleとrun間変動を残した。
測定loop時間はwarm-upのin-flight tail、最後の回収とCPU内容照合も含み、GPU区間とは異なる。
clock・affinity・外部負荷は固定しておらず、この小さな基準負荷を代表sceneの性能や表示FPSへ一般化しない。

3 runともwarm-up後のnative作成数はresource 18 / execution 3 / descriptor arena 3で増加なし。
各runの160 frameでresource再利用785回、execution/descriptor再利用各157回を確認した。
frame回収後・永続meshとhistoryをまだ登録している時点のメモリは次のとおり。

| 指標 | 3 runで共通の値 |
|---|---:|
| 登録済み論理byte | 100 |
| pending等で保持中の未登録論理byte | 0 |
| 再利用cacheの論理byte | 589,836 |
| VMA Allocation | 20件、9,177,200 byte |
| VMA確保block | 3件、100,663,296 byte（96 MiB） |
| upload予約high-water | 256 byte |
| readback予約high-water | 196,864 byte |
| 回収後upload / readbackのlive range | 両方0 |

正しさと同じbyte/画素照合を性能runでも実施し、native破棄後の診断件数はゼロだった。
正しさのRequired runではCore/SyncValの実効値と終了後診断を確認した。
性能用Disabled要求でLayer起動報告がない場合も、外部Layerが絶対にない証明とは扱わず、
要求・観測値・環境変数をJSONへ残した。今回の性能runは起動報告なし、実効値は未確認のnullである。

## RenderGraphのAPIとcache改善

`renderer-lup`でowner検査付きの`Graph::image_desc`を追加し、custom処理の幅・高さの別渡しを除去した。
`renderer-bcf`でGraphErrorの8分類、pass/resource・名前・範囲、実際の有向循環のcontextを追加した。
CPUのみで一時/import画像とdepth、foreign handle、未初期化の穴とexport、同名passと循環の後続tail、
構築前のpass名、customエラーを確認した。旧struct literalは`GraphError::custom`へ移行した。
互換性維持を既定の制約にしない方針は`AGENTS.md`に明記した。

`renderer-vxf`は同じdescriptorの再利用を優先し、件数上限までは異なる条件も保持する。
上限時の退避は最古の受理serial・同値ならindex順で、登録中・pending lease・世代上限を保護する。
追加のstrict GPU回帰4件で、2×2/3×3画像の交互8回が**新規2・再利用6**になること、
容量圧力、世代、未観測pending、置換先の命名失敗後の旧allocation維持と再利用を確認した。
命名失敗はnative image/view作成後の部分失敗であり、実driverのOOM注入試験ではない。

cacheの性能比較は変更前`55f94ee`と、cache修正`27fbd83`相当のライブラリを静的に組み込んだ同じprobeで行った。
release・Disabled要求・1 execution slot・容量8、64×64固定／64×64と96×96交互、
warm-up 32＋測定128を各3 run。各submit後に明示Fence待機し、全byteを照合する制御されたcache試験で、
継続frameのthroughputとは異なる。CPU activeはbuild〜submit復帰で、Fence待機を含まない。
before→after順に測り、clock/affinity/電力/OS負荷は固定していない。

| 交互サイズの指標 | 変更前 | 変更後 |
|---|---:|---:|
| CPU activeのrun中央値 | 25.90–26.80 µs | 15.00–16.10 µs |
| CPU prepareのrun中央値 | 5.90–6.25 µs | 1.70–1.80 µs |
| warm-upを含む160回の新規作成／再利用 | 160 / 0 | 2 / 158 |
| 終了時cacheの論理byte | 36,864 | 53,248 |
| VMA Allocation件数／byte | 3 / 8,437,760 | 4 / 8,454,144 |
| VMA block件数／byte | 3 / 100,663,296 | 3 / 100,663,296 |

固定サイズcontrolのCPU active中央値は変更前14.80–16.10 µs、変更後15.30–16.35 µsで範囲が重なる。
交互サイズのGPU Graph中央値も変更前6.368–6.640 µs、変更後6.320–6.448 µsで、GPU高速化は主張しない。
再利用と引き換えに保持allocationは増える。block容量が今回増えなかったことを、VRAM budgetや全負荷の保証にしない。

### Draw listの本実装と比較計測

**標準の不透明描画はbatch経路を既定としてmainへ統合した。**
非空draw listは一つのGraph Raster操作とnative描画区間にし、color/depthを一度clearする。
draw順・MVP/tint・各drawのindex/provenance検査・最終Storeを維持し、空listはclear-onlyとする。
単一drawの`Graph::raster`は参照経路として残し、公開PassEncoder・任意shader・汎用パス統合は追加していない。

同じmeshだけでなく、多数の異なるmeshに対応するため、Graph importのcanonical索引とnative用途集約を
順序付きmapにした。compile側のパス内競合検査はwriter列を使う検索へ変更し、入力全組合せの走査を除去した。
CPUで4096通りの独立pairwise参照との比較、8192入力、重複read、半開区間、失敗contextを確認した。
さらにRasterノード数、128種類のmesh producer、構築失敗とimport同一性を検査した。
strict GPU試験ではscalarとの全RGBA一致、空draw、逆順、同深度、異なるmesh/行列/tint、後続Compute/composite、
後半drawの不正index、同じindex bufferで頂点範囲が異なるdraw、共有Vertex/Indexのscope合成、
96 draw・3 meshのpending中更新/削除と所有の維持を確認した。独立static reviewで追加の修正指摘はなかった。

比較sourceはscalar `bf0d61c`（`776cbab`＋distinct計測ハーネス）と本実装`97d41cc`。
実装のRustコードを変更せず、両方cleanな状態でreleaseを作り、実行時のsource commitと実行ファイルhashを記録した。
共有1 meshとdrawごとに独立したmesh資源を使うdistinctの両方で、同じ3頂点/3 indexの小さい三角形を描く。
draw数0/1/8/32/128/512/1024、overlap 64×64とgrid 512×512で、計28負荷を使った。
history/custom/churnなし、永続mesh、3 in-flight、Disabled要求、warm-up 32＋測定128 frameを各3 run。
各負荷でscalar→batch、batch→scalar、scalar→batchの順に単独実行した。
全168 run・21504測定frameでGPU timestamp availableとwarm-up後のnative作成数安定を確認した。
別のRequired/Core/SyncVal有効56 runでは各8 frameを照合し、28負荷すべての最終RGBAが全byte一致した。
測定frameのchecksumも配置/draw数ごとに両実装・mesh条件・全runで同一だった。

以下はrunごとの中央値の範囲。単位はµs。CPUはpollを除くbuild〜submit復帰の直接計測、
GPUはcompositeとreadbackを含むGraph全体で、draw数自体の削減やinstancingは行っていない。

| 負荷 | CPU scalar | CPU batch | GPU scalar | GPU batch |
|---|---:|---:|---:|---:|
| shared grid・8 draw | 56.75–59.65 | 36.70–38.65 | 70.304–70.336 | 54.400–54.496 |
| shared grid・128 draw | 394.20–423.25 | 51.50–52.45 | 314.592–315.408 | 56.128–56.144 |
| shared grid・1024 draw | 3303.05–3387.45 | 141.90–214.80 | 1914.976–1917.072 | 68.512–68.544 |
| distinct grid・128 draw | 679.05–736.00 | 381.45–408.00 | 315.056–315.136 | 55.840–55.872 |
| distinct grid・1024 draw | 6814.90–6977.00 | 3349.95–3521.70 | 2156.848–2171.040 | 71.232–71.280 |
| distinct overlap・1024 draw | 6720.30–6874.55 | 3332.35–3389.15 | 1636.272–1848.432 | 36.048–36.112 |

gridの8 draw以上ではshared/distinctとも全runでCPU中央値・p95とGPUが改善した。
distinctではCPU側に多数の資源のsnapshot・検証・命令準備の負担が残り、sharedと同じCPU時間にはならない。
1024 distinctの物理entry容量は2057、warm-up後の作成数も2057で安定した。
最後の統計はmeshを登録中に取得しており、登録済み論理byteはshared 96、distinct 98,304。
その後に全meshを削除し、native所有者の破棄後まで診断ゼロを確認した。

小規模sceneのCPU tailの変動は引き続き残す。
通常条件のshared grid 0 drawはscalarの中央値34.15–35.85 µs／p95 46.10–69.20 µsに対し、
batchは中央値33.25–55.00 µs／p95 43.60–214.50 µsだった。overlap 0 drawでも遅いrunが双方に現れた。
0 drawはどちらもclear/composite/readbackへ進むが、変動の原因をschedulerと断定していない。
別条件の追加controlでは子processを同じ論理CPU（affinity mask 1）へ固定し、
sharedの0/1 draw・両配置をwarm-up 64＋測定512 frame、各3 runの交互順で測った。
overlap 0 drawの中央値はscalar 15.90–16.50 µs、batch 16.20–16.40 µs、
p95は19.10–23.00 µsと19.10–21.20 µsだった。
grid 0 drawの中央値は32.80–34.90 µsと32.55–32.90 µs、p95は40.80–63.60 µsと41.40–75.80 µsだった。
1 drawは両配置の全runでbatch中央値が小さかったが、p95の変動は残り、通常desktopでの非回帰は保証しない。

利益はGraph粒度、import/競合/用途検索、barrier、描画scope、状態設定を合わせた変更の結果で、
begin/end削減だけへ帰属しない。CPU readback照合とchecksumもloop時間へ影響するため、FPS倍率とは呼ばない。
distinctも同じ小さいgeometryの独立資源であり、重いmesh、別GPU/vendor、tile GPU、任意shaderの性能は未確認。
[比較JSON](rendergraph-metrics.json)のschema 2に環境・source/実行ファイルhash、全runの統計・生sample、
保持メモリ、affinity control、RGBAのSHA-256を保存した。以前の試作測定はGit履歴に残す。
cache単独変更時の比較は同じJSON内に元のsourceと条件で保持する。
性能runの実効Core/SyncValは未観測のnullであり、Disabled要求と外部Layerが絶対にない証明は区別する。

### 初回GPU実行のnative異常

前回の`ba91f3b`での最初のpresentation全GPU実行は、buffer試験後のDevice smoke開始付近で`0xc0000005`により終了した。
Windows Application Errorのmoduleはunknown、offsetは`0xcb`で、直前のValidation警告・エラーはなかった。
同時期に試作側のGPU suiteも実行していたが、並行実行を直接原因とは断定していない。
Device単独、その後のpresentation全30件を2 run、headless全29件、上記比較と例の単独実行では再現しなかった。
失敗ログを保存し、原因未確定の観測を`renderer-0pb`で追跡する。driver原因や解決済みとは扱わない。
batch本実装`97d41cc`のheadless 33件・表示込み34件、56比較runと各例でも再現しなかった。
今回の`2e543b4`のheadless 40件・表示込み41件・公開負荷1件、release検証・計測と各例も単独実行し、再現していない。

## エンジン負荷と入力・資源管理

最終ライブラリソースe96d270をreleaseで確認した。
大meshは400×400 cell、160,801頂点・960,000 index、8,342,428 byte。
別meshと合わせた11,764,664 byteを256 KiB stagingから47 chunkで受理し、
最後の受理からCPU完了待ちなしで描画へ進む。頂点／index byteの照合も含む。

engine_workloadsのverifyは開発検証・native Core＋SyncValありで45画像、
benchmarkはRenderer診断なし・native Disabledで519画像が全RGBA期待値と一致した。
32個のCPU assetを保持した4-mesh cacheの入れ替え、3 pending中の更新・削除／再登録、
2 Viewの先行build、独立renderer入力を含む。verifyはnative破棄後まで診断ゼロ。
製品runのnative実効フラグは未観測としてnullを記録した。

変更前d61ca29と製品構成を、32 warm-up＋128測定frame×3 run、3 in-flightで比較した。
大meshを64×64へ描くwarm経路で、CPU activeはbuild開始からsubmit復帰まで、
cold upload・snapshot作成・容量待機を含まない。GPUはreadback込みで表示・VSyncを含まない。

| 指標 | 変更前 | 製品構成 |
|---|---:|---:|
| CPU active p50／p95 | 56.0／93.2 µs | 21.3／49.7 µs |
| GPU Graph p50／p95 | 111.072／112.128 µs | 110.688／114.240 µs |
| cold upload受理／完了観測 | 5.623／5.673 ms | 7.291／7.369 ms |

検証・診断の設定を意図的に変えた作業環境サンプルで、個別修正の因果効果やGPU高速化を示さない。
clock・affinity・電力・OS負荷を固定せず、統計的な有意差やFPSを主張しない。
cold uploadは事前生成済みmeshの転送操作で、level load全体の時間ではない。
最終常駐4 mesh／19,600論理byte、staging high-water 262,144 byte、
VMA Allocation 12,459,488 byte、block 100,663,296 byteは前後で同じだった。
trimは明示APIであり、この負荷へ暗黙のevictionを追加していない。

条件・生sample・ソースhash・再構築可能な通常ライブラリallocation probeとLLVM文字列検査結果は
[現在のエンジン負荷データ](engine-workloads-metrics.json)へ保存した。
以前の登録／cache索引化比較と出力hashはGit履歴に残す。

## 第二APIの準備とbuffer実行

M4の第二APIはD3D12を選定した。既存Windows実機とshaderツールを使って、
同じ入力・Graph・GPUでVulkanと比較しやすいことが理由である。
準備4課題（`renderer-we8.1`・`.2`、`renderer-6qc`・`renderer-k51`）に続き、
M4-03の公開`D3D12Runtime`とM4-04の共有Graph buffer実行を追加した。
Windows限定の`d3d12` featureで、単一DIRECT queueのupload／copy／readbackを使える。
画像、import/export、上限付き再利用と一般RHIの実装は後続課題へ残す。

bindingsは`windows-bindgen`／`windows-default`／`windows-core` 0.100.0、MSRV 1.95で固定した。
入力`Windows.Win32.winmd`のSHA-256は`818706A4908FE51447EBFD88F1E54195CEDA5EC437F95FE17F1250471680811D`、
M4-03/04で必要APIのfilterを拡張したLF固定の生成Rustは
`05ADD69A1266EF2B3EF24D73CDCAC193A0D603B441ACCEDD6D1B1EB7A490D9C0`で再生成一致した。
通常ビルドは生成済みRustを使い、generatorとmetadataの依存は独立workspaceで監査する。
既存依存の`windows-link` 0.2と新規0.100.0の併存、tool監査の未使用license許可に関するwarningは
GPU Validationとは別の依存監査診断として確認した。ライセンスやadvisoryの除外追加はない。

固定SDK 1.4.341.1のSPIRV-Cross 1.4.341.0とDXC 1.9.0.5180で5種類のshaderを生成・検証し、
既存SPIR-V、HLSL、DXIL、binding／定数reflection、manifestの再生成一致を確認した。
固定toolのhash・引数と4／80-byte定数、行列・座標・samplerの対応は
[ツールガイド](../guides/tooling.md#d3d12の固定shader生成)と管理下metadataに記録した。
生成物の改変とSDK不足が失敗することも確認した。

準備段階のソース`e96d270410db3f528bfec2df9ff067d6200c489d`での3回成功に加え、
統合ソース`2b15cd0bc7ca68892f433c1a67d5033278f7252f`、clean mainで
[準備スクリプト](../../scripts/verify-m4-preparation.ps1)を1回再実行して成功した。
機材・実行担当・再実行方法は[開発手順](../guides/development.md#m4のd3d12準備と実gpu受入経路)を参照する。

| 確認 | 準備probeの結果 |
|---|---|
| GPU同一性 | NVIDIA RTX 3060、vendor/device IDとDXGI／Vulkan LUIDが一致 |
| 能力 | SM6.0、RGBA8 typed UAV load/store、resource binding tier 3、heap tier 2 |
| allocation基準 | DEFAULT／UPLOAD／READBACKの64-byte committed bufferを作成・検査・破棄。各allocation 65,536 byte、alignment 65,536 |
| Debug Layer／GBV | Device作成前に設定、InfoQueue取得と起動message ID1016のGBV有効報告を確認。設定getterだけから推測していない |
| 終了後診断 | 最終Device Releaseを含むcallbackとDXGI global queueを確認。warning／error／corruption、callback失敗、破棄メッセージはゼロ |
| Vulkan検証 | 実選択Deviceを使い、Core／SyncValの実効と破棄後の診断ゼロを確認 |

初期storage filterはINFOだけを65件抑制していた。filterのallow／deny全条件を検査し、
警告・error・corruptionを抑制しないことを確認してから解除した。元のfilterと件数はJSONへ残し、
解除後の追加抑制はゼロ。既存メッセージを消去して成功扱いにはしていない。
最初のprobeで検出したDEFAULT bufferの初期状態に関する警告1328は`COMMON`への修正で解消し、例外は追加していない。

M4-03/04の通常経路は、Graph構築／compileの不変保証と実行順を共有し、
D3D12でlive操作の対応可否と作成上限をprepare時に確定する。submit時に同じGraph内容を再走査しない。
DEFAULTは`COMMON`から明示legacy transitionを記録し、同一buffer内の非重複copyはscratch経由で実行する。
1 bufferの2 GiB制限はライブラリの対応上限であり、実native allocation成功やVRAM予算の保証ではない。
DeviceのCOM所有共有、processで固定した検証設定、最終所有者の解放順とcallback寿命は独立レビューで確認した。
診断なしではnative診断module・Session fieldを除外し、monitorはZSTとなることをCPU試験でも確認した。
同期計画、native作成失敗、Fence、Device異常、GPU利用中の所有は製品にも残る。

最適化済みの厳格runtime試験では、共有する二つのruntimeの先行shutdown、設定不一致の拒否、
最終shutdown後の同一process内再作成と再終了が成功した。
buffer寿命試験では未投入キャンセル、最初の実committed resource作成後の注入失敗、
二つの投入の逆順待機、runtimeを先に終了したpending、pending Drop、readbackのowner／二重消費拒否、
readbackなしの空Graphについて正常完了と最後のnative参照の解放順を確認した。
別途CPU試験で、voidのExecuteより前の所有確立と後続Signal失敗／unwind時の保持を確認した。
実native OOMやDevice removalを発生させた試験ではない。Device部分初期化の解放はRAIIと静的レビューで確認した。

[二API反復スクリプト](../../scripts/verify-m4-buffer.ps1)は同じcleanソースで
`-Runs 3 -OutputDirectory .tools/m4/runtime-buffer/acceptance-final`を実行した。
各構成は別processのreleaseで、各回VulkanとD3D12のGPU作業を逐次実行した。

| 比較 | 3回共通の結果 |
|---|---|
| 実adapter | RTX 3060、vendor `4318`／device `9351`、両APIの有効LUID `[116,226,0,0,0,0,0,0]`が一致 |
| 入力 | 一度compileした同じ10-pass Graph。3 upload、4 copy（同一buffer内1件）、3 readback |
| 内容 | 部分offsetと未変更領域を含む308 byte／backendが、期待値・Vulkan・D3D12で全一致 |
| 厳格構成 | `vulkan,d3d12,development-validation`。Debug Layer／GBV、Core／SyncValの実効を確認。最終解放までwarning／error／corruption／callback失敗／問題本文欠落ゼロ |
| 製品構成 | `vulkan,d3d12`。開発検証・両native診断・Graph診断・debug assertionsが無効。明示native Disabledで同じ全byte一致 |
| 証拠 | `summary.json`、各構成のfeature tree、6回分のJSON／ログに原byte・checksum・実効設定・終了診断を保持 |

製品構成の診断件数0は観測を組み込まない結果であり、外部からの検証注入がなかった証明には使わない。
GBVを有効にしてbuffer copyを確認したが、shaderの計装を実行した証拠ではない。
画像等価性と二APIの性能比較、長時間耐久、実VRAM飽和は未実行で、性能優位や閾値を受入条件にしていない。
旧M4-04までの6／22課題を確認した実績であり、旧M4全体や新Bistro計画の完了ではない。
実GPU CIは追加せず、既存CIはCPU／buildと依存監査を担当する。

別GPU/driver、Linux表示・実GPU、Mac/Metal、実native OOM/Device loss/Surface loss、
拡張なしWSI、透明Window、HDR、物理aliasing、複数queueは今回の追加実機確認に含まない。
上記の追加affinity controlを除き、clock・affinity・電力・OS負荷は固定せず、他エンジンとの性能比較も行っていない。
過去のtransfer-only Criterionや局所compiler比較の数値を、現在のframe基準値やFPSに読み替えない。

## 再現物

今回の10構成の品質・release両構成・独立feature・Rustdoc・実GPU・表示例のログは`.tools/graph-review/gates/`、
3回反復の環境・clean source commit・shader hash・全probe JSON／logは
`.tools/graph-review/preparation-final/`へ保存した。最初のPython経由起動はPowerShell module探索で失敗し、
その証拠は`.tools/graph-review/preparation/`に保持した。PowerShellからの直接起動で前提検査を省かず成功した。
再実行sourceと生成済みbindings／shaderはGit管理し、`.tools/`だけに依存しない。

M4前の入力・資源管理のゲートは`.tools/engine-readiness/gates/`へ保存した。
`{cpu,vulkan,presentation}-{check,clippy,test}.log`、`fmt.log`、`examples-test.log`、
`gpu-{vulkan,presentation}.log`、`gpu-game-public.log`、`workloads-{verify,benchmark}.log`、
`frame-verify.log`、`distinct-verify.log`、`headless.log`、`windowed-{on,off}.log`を含む。
現在の負荷とCPU保持量probeは[エンジン負荷JSON](engine-workloads-metrics.json)に収めた。以前の索引化probeはGit履歴を参照する。
大メッシュの再実行は開発ガイドの`engine_workloads`を使う。性能値だけの固定合否閾値はCIに追加していない。

今回のゲートは`.tools/opaque-batching/gates/{cpu,vulkan,presentation}-{check,clippy,test}.log`と`fmt.log`、
`examples.log`、`gpu-{vulkan,presentation}.log`、`default-verify.log`、`windowed-{on,off}.log`。
比較の各run JSON/RGBAと追加controlは`.tools/opaque-batching/{comparison,controls}/`へ保存した。
共有する条件・統計・生sampleは[RenderGraph比較JSON](rendergraph-metrics.json)を正とし、比較sourceはmainのGit履歴から取得できる。
cache比較のprobeとログは`.tools/pre-m4/`にあり、前回のnative異常は同ディレクトリの
`gates/gpu-all-initial-native-crash.log`、単独Device確認は`gates/gpu-device-isolated.log`へ残した。
ローカルの`.tools/`自体を共有sourceの代わりにはしない。
既存GLSL／SPIR-V binaryは今回も変更せず、固定SDKで再生成一致を再確認した。
今回追加したD3D12依存・生成tool依存・DXILの検証結果は上記M4準備のログを参照する。

従来のM3ゲートは`.tools/m3/gates/{cpu,vulkan,presentation}-{check,clippy,test}.log`、
`gpu-all.log`、`windowed-smoke.log`、`example-cpu.log`、`shader-verify.log`、`headless-example.log`。
Linux CI修正後の追加確認は`ci-fix-{vulkan,presentation}-{check,clippy,test}.log`と`ci-fix-gpu.log`。
統合verifyのJSON/PPMは`.tools/m3/frame-verify.json`、`frame.ppm`、
WSI能力の元情報は`.tools/m3/preflight-vulkaninfo.log`、第二API probeは`.tools/m3/second-api-probe.txt`。
`.tools/`はGit対象外で、再実行方法は開発ガイドに記載する。
性能runの生ログは`.tools/m3/gates/frame-baseline.log`、Gitで共有する基準データは
[current-metrics.json](current-metrics.json)を正とする。

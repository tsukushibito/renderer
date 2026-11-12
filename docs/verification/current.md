# 現行実装の検証状況

最終確認日: 2026-11-13。実行手順は[開発ガイド](../guides/development.md)、
現在の契約は[RenderGraph](../design/rendergraph.md)と[Vulkan設計](../design/vulkan.md)、
全体範囲は[実装方針](../design/implementation-policy.md)を参照する。
過去の検証表・比較はGit履歴に残し、この文書は現在の確認範囲を示す。

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

CPUのGraph・engine入力はGPU・windowなしで構築できる。
表示はWindows、EXT swapchain maintenance1と依存拡張・feature、
同一Graphics＋Compute＋present queue、SRGB_NONLINEARのRGBA8/BGRA8_SRGB、OPAQUE対応に限定する。
実機で必要能力を照会した。固定bindingsにあるEXT版を利用し、headlessの必要能力は増やしていない。
別queue、拡張なしfallback、透明Window、HDR、任意shaderは対応したと扱わない。

## 品質ゲート

| 確認 | 結果 |
|---|---|
| fmt | 成功 |
| CPU / Vulkan / Windows presentationの全target check・Clippy `-D warnings` | 3構成すべて成功 |
| CPUのみの通常試験 | 単体43件＋公開API 9件＋負荷fixture 3件成功 |
| Vulkan featureの通常試験 | CPU単体143件＋公開API 9件＋負荷fixture 4件成功、42 ignored |
| presentation featureの通常試験 | CPU単体148件＋公開API 9件＋負荷fixture 4件成功、43 ignored |
| 明示headless実GPU試験 | 40件成功、3.78秒 |
| headless＋実Win32 Surfaceを含む明示GPU試験 | 単独実行41件成功、4.53秒。さらに公開APIのoffscreen負荷1件成功、0.57秒。native異常は再現せず。前回の観測は後述 |
| Validation / SyncVal | 実効設定を確認し、native所有者の破棄後までwarning・error・callback失敗ゼロ |
| Shader再生成検証 | 以前の固定SDKによる5種類のspirv-val・再生成SHA-256一致を継承。今回shader変更なし |
| headless例 | 従来の128×96描画・深度・Compute・合成とPPM出力成功 |
| engine headless統合例 | 24 frame、3 in-flight、custom処理・history・更新/削除・遅延readbackの照合成功 |
| engine Windows表示例 | ON/OFF各起動の`windowed --smoke`で12 Present要求、VSync往復切り替え・resize・最小化復帰・pending終了、終了コード0 |
| 表示例の引数検査 | VSync既定ON、ON/OFF指定、smoke併用、不正引数拒否のCPU試験1件成功 |
| exampleのCPU試験 | frame計測9件、大メッシュfixture・統計4件、windowed引数1件成功 |
| 依存監査 | cargo-deny 0.20.2でadvisories / bans / licenses / sources成功 |
| 文書 | UTF-8、相対リンク・anchor、fenced block、差分空白を確認 |

実GPUはignored試験を明示実行し、前提不足を成功・skipへ置き換えていない。
CPU fallbackの成功で実GPU受入を代替しない。
入力拒否を確認するアプリケーションERRORログはValidationの異常とは区別する。
最終品質ゲートでは`CARGO_INCREMENTAL=0`を使い、初期の並行確認で見られた
hard-link不可というincremental cacheの環境warningをソース診断と区別した。

Windows / LinuxのCPU・Vulkan、およびWindows presentationのcheck・Clippy・CPU試験を
[GitHub Actions](https://github.com/tsukushibito/renderer/actions/workflows/ci.yml)に設定している。
このローカル確認はLinux実行や実GPU CIの合格を意味しない。

### VSyncの選択・切り替え

`renderer-5kp`で`PresentationConfig::vsync`、`Presenter::set_vsync`と要求値getterを追加した。
既定ONはFIFO、OFFは実Surface対応を確認したIMMEDIATEで、未対応要求は`Unsupported`となる。
WSIの能力照会・同期・退役世代の指針と単一モードSwapchainのAPI契約に従い、
既存の再作成とbounded retirementを利用した。通常経路への全体idleや、MAILBOXへのfallbackは追加していない。
IMMEDIATEの性能・latency改善を実測した変更ではなく、利用側がVSync方針を選べる機能である。

CPU試験では対応モードの選択、MAILBOXがあっても未対応OFFを拒否することを確認した。
実Win32試験では同値要求で再作成しないこと、presentの所有権保持中のFIFO→IMMEDIATE→FIFO、
最小化中の変更保留、resize・取得後cancel・pending終了、close後の変更拒否を確認した。
Graph側の画素照合と、全native所有者を破棄した後までCore/SyncValの診断ゼロを維持した。
通常3構成のcheck・Clippy・CPU試験と全GPU26件、表示例の両起動モードを再実行した。
Shaderは変更せず再生成検証は前回結果を継承する。依存の追加・更新はない。

Vキー入力はexampleのhandlerに接続し、同じ切り替え関数をsmokeから自動実行した。
実際のキー入力の手動試験、別GPU／driverでの両モード、driver／compositor設定の影響、
表示FPS・PresentMonの記録は未検証である。FPSの推奨計測境界は[開発ガイド](../guides/development.md#表示fpsの計測方針)にまとめた。
今回のログはGit管理外の`.tools/vsync/`に保存した。headlessの既存性能基準値は更新していない。

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
M3のGraph構築拡張をその完了として扱わない。公開一般PassEncoderは第二APIで検証するM4へ残す。

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

## エンジン負荷とM4前の入力・資源管理

検証したRust実装は`2e543b40c03f857618a8bd7126826fd1c6543a1c`。
`renderer-m5y`／`km2`／`7mi`／`t99`／`v0s`／`9ve`／`b6n`／`2d0`で、
共有CPU asset入力、独立したGPU常駐、分割upload、内容versionと同期状態、索引化、
直接出力、一件待機、present outcomeを修正した。API互換用の別経路は増やしていない。

CPU assetを削除しなくてもGPU常駐を退避でき、件数・論理byte予算とView用物理枠を守る。
物理容量4・論理mesh上限16でCPU側にA/B/Cを残した入れ替えが進み、資源作成は4件で安定した。
prepared pinで枠が不足する場合はエラーにし、解放後に進むことも確認した。
共有meshを使う2 Viewを先にbuildしてから順次submitし、異なる寸法・行列・tintとComputeの
全RGBAを照合した。同一snapshotは元のCPU mesh/material削除後も別rendererで利用できた。

大meshは400×400 cell、160,801頂点・960,000 index、8,342,428 byteで、vertex入力だけでも4 MiBを超える。
別の256×256 cellのmeshを加えた11,764,664 byteを、256 KiB stagingから47 chunkで受理した。
単体meshの全vertex/index byteは小さいreadback領域に分けて照合した。
最後のupload受理直後、Fence完了を観測せずに描画できることも確認した。
chunkは元の`Arc`とsource範囲を共有し、CPU側に永続的なbyte複製を増やさない。

releaseの`engine_workloads --verify`では45画像、明示Validation無効の`--benchmark`では519画像が
全RGBA期待値と一致した。両方とも32個のCPU assetを保持した4-mesh cacheの入れ替え、
3 pending frame中の更新・削除／再登録、複数View、独立renderer入力を含む。
verifyはnative破棄後までCore／SyncValが有効で、warning・error・callback失敗ゼロ。
Disabled要求時の実効フラグは未確認としてnullを記録し、Layer非注入の証明とはしない。

性能測定は32 warm-up＋128測定frameを3 run、3 in-flightで単独実行した。
大meshを64×64へ描くwarm経路で、cold upload・snapshot作成・容量待機をCPU activeへ含めない。
GPU区間はreadback込み、表示・VSyncは含まない。p50／p95はnearest-rankで算出した。

| 指標 | 各runのp50 | 各runのp95 |
|---|---:|---:|
| CPU build開始〜submit復帰 | 21.7–24.0 µs | 28.4–35.0 µs |
| GPU Graph | 110.816–110.880 µs | 111.136–111.328 µs |

pooled 384 sampleのCPU p50／p95は23.3／32.0 µs、GPUは110.816／111.232 µs。
submit復帰後から完了観測までのlatency p95は335.4 µsで、scanoutや総frame latencyではない。
benchmarkのcold upload受理は5.542 ms、完了観測まで5.596 msだった。
転送前のmesh生成・byte化を含むlevel load時間ではなく、明示waitを選んだstartup操作の値である。
staging予約high-waterは262,144 byteで容量内、最終常駐は4 mesh／19,600論理byte。
VMA Allocationは12,459,488 byte、blockは100,663,296 byteで、常駐予算やprocess VRAMとは別の値である。

CPU索引化比較は、公開Engine登録・Graph exportと、production CacheIndexを含むmetadataモデルを分けた。
変更前`7b851c6`、変更後のCPU実装相当`5fd39cf`／cache索引`ab48f57`、release、AB／BA／AB、
各case・processで7 sample、合計882 sample。入力生成と最終破棄を計測外とした。

| 8192件のCPU処理・run中央値 | 変更前 | 変更後 |
|---|---:|---:|
| mesh登録 | 13.388–13.509 ms | 0.376–0.383 ms |
| material登録 | 17.580–17.733 ms | 0.0965–0.1049 ms |
| Graph export宣言 | 3.732–3.787 ms | 0.0621–0.0872 ms |
| cache metadata：8192登録＋1024一時資源 | 2.025–2.038 ms | 0.1949–0.1995 ms |

小さいcacheモデル（32登録＋32一時資源）では0.5–0.6から3.6–3.7 µsへ管理費が増えた。
このモデルはArc・通知lock・native作成・GPU実行を含まず、frame時間の比較へ読み替えない。
clock・affinity・電力・OS負荷は固定していない。固定shader／UNORMの結果で、PBR・多vendor・FPSの保証ではない。

既存のchurn付き24-frame出力は前回保存したRGBAと全byte一致し、128 distinct drawの512×512 gridも
前回公表したRGBA SHA-256と一致した。無変換composite除去で出力を変えていない。
全sample・条件・CPU probe本体とhash・出力hashは[エンジン負荷データ](engine-workloads-metrics.json)へ保存した。
CPU probeは埋め込みsourceから再構築でき、ローカル`.tools/`だけを再現用sourceにしていない。

## 第二APIの選定と未確認範囲

M4の第二APIはD3D12を選定した。既存Windows実機とshaderツールを使って、
同じ入力・Graph・GPUでVulkanと比較しやすいことが理由である。
D3D12CreateDevice(default adapter, FL11_0)とID3D12Debug取得はS_OK、直後にreleaseし命令投入なし。
adapterの識別、Debug Layer/GBV有効化、実描画と画像等価性、実GPU CIは未確認。
準備は`renderer-6qc`、`renderer-k51`に登録し、M3で第二backend実装や一般RHIを先行固定していない。

別GPU/driver、Linux表示・実GPU、Mac/Metal、実native OOM/Device loss/Surface loss、
拡張なしWSI、透明Window、HDR、物理aliasing、複数queueは今回の追加実機確認に含まない。
上記の追加affinity controlを除き、clock・affinity・電力・OS負荷は固定せず、他エンジンとの性能比較も行っていない。
過去のtransfer-only Criterionや局所compiler比較の数値を、現在のframe基準値やFPSに読み替えない。

## 再現物

M4前の入力・資源管理のゲートは`.tools/engine-readiness/gates/`へ保存した。
`{cpu,vulkan,presentation}-{check,clippy,test}.log`、`fmt.log`、`examples-test.log`、
`gpu-{vulkan,presentation}.log`、`gpu-game-public.log`、`workloads-{verify,benchmark}.log`、
`frame-verify.log`、`distinct-verify.log`、`headless.log`、`windowed-{on,off}.log`を含む。
共有するデータとCPU probe sourceは[エンジン負荷JSON](engine-workloads-metrics.json)に収めた。
大メッシュの再実行は開発ガイドの`engine_workloads`を使う。性能値だけの固定合否閾値はCIに追加していない。

今回のゲートは`.tools/opaque-batching/gates/{cpu,vulkan,presentation}-{check,clippy,test}.log`と`fmt.log`、
`examples.log`、`gpu-{vulkan,presentation}.log`、`default-verify.log`、`windowed-{on,off}.log`。
比較の各run JSON/RGBAと追加controlは`.tools/opaque-batching/{comparison,controls}/`へ保存した。
共有する条件・統計・生sampleは[RenderGraph比較JSON](rendergraph-metrics.json)を正とし、比較sourceはmainのGit履歴から取得できる。
cache比較のprobeとログは`.tools/pre-m4/`にあり、前回のnative異常は同ディレクトリの
`gates/gpu-all-initial-native-crash.log`、単独Device確認は`gates/gpu-device-isolated.log`へ残した。
ローカルの`.tools/`自体を共有sourceの代わりにはしない。
GLSL/SPIR-V binaryと依存は今回変更せず、Shader再生成とローカル依存監査は既存結果を引き継ぐ。

従来のM3ゲートは`.tools/m3/gates/{cpu,vulkan,presentation}-{check,clippy,test}.log`、
`gpu-all.log`、`windowed-smoke.log`、`example-cpu.log`、`shader-verify.log`、`headless-example.log`。
Linux CI修正後の追加確認は`ci-fix-{vulkan,presentation}-{check,clippy,test}.log`と`ci-fix-gpu.log`。
統合verifyのJSON/PPMは`.tools/m3/frame-verify.json`、`frame.ppm`、
WSI能力の元情報は`.tools/m3/preflight-vulkaninfo.log`、第二API probeは`.tools/m3/second-api-probe.txt`。
`.tools/`はGit対象外で、再実行方法は開発ガイドに記載する。
性能runの生ログは`.tools/m3/gates/frame-baseline.log`、Gitで共有する基準データは
[current-metrics.json](current-metrics.json)を正とする。

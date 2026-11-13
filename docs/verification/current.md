# 現行実装の検証状況

最終確認日: 2026-11-13。実行手順は[開発ガイド](../guides/development.md)、
現在の契約は[RenderGraph](../design/rendergraph.md)と[Vulkan設計](../design/vulkan.md)、
全体範囲は[実装方針](../design/implementation-policy.md)を参照する。
過去の検証表・比較はGit履歴に残し、この文書は現在の確認範囲を示す。

## Bistro計画と現行実装の区別

開発計画を[実装方針のB0〜B5](../design/implementation-policy.md#bistro-roadmap)へ改訂した。
追跡は`renderer-7ft`。`renderer-7ft.1.1`では利用者要件を合意し、
[確定要件と取得前の仕様案](../design/implementation-policy.md#bistro-requirements)を記録した。
対象は屋外全体、太陽光＋IBL＋影、PBR/HDR/tone mappingと利用側Bloom、基準1920×1080、
自由カメラと固定視点・自動巡回、両APIのWindows表示・操作、初回受入は既存RTX 3060である。
要件の合意を実装・実GPU受入の成功とは扱わない。
既存基盤の再利用は必須にせず、B0で維持・改修・置換・外部基盤を比較する。
旧M4の6/22完了は準備・Device・buffer実行の実績であり、新計画の進捗率へ流用しない。
旧M4の未着手分は後継課題を示して置換し、実装完了とは扱わない。

今回の変更は文書とBeadsの要件・仕様案だけで、下記のGPU・CPU実行結果は従来の固定ソースでの実績である。
NVIDIA公式の配布形式・license案内と、変換・PBR/IBL・影・tone mappingの一次資料を確認した。
素材実体の配布hash・mesh/material/texture棚卸し・変換再現性・IBL素材・camera/light具体値は未確認で、
`renderer-7ft.1.1`は進行中である。次の実作業は素材実体に基づく調査・変換の実証と条件の固定である。
Bistroの素材取得・変換、テクスチャ付きPBR・影・HDR、独自Bloom、公開パスAPIの二API実証とD3D12表示は未実行である。
画像閾値の校正・性能予算・RTX 3060での全体収容も未実測であり、出発案を性能保証へ読み替えない。
文書4件のUTF-8・fenceと相対リンク/anchor 71件・既存の実装方針へのbacklink、`git diff --check`、
Beadsの依存循環なしを確認した。独立レビューで検証構成の表現と開発手順に残っていた旧M4の将来指示を修正した。
実装・shader・依存・ビルド設定の変更がないため、今回のためのローカルGPU/CPU試験は再実行しない。

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

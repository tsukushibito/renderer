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
| CPUのみの通常試験 | 単体25件＋公開API 3件成功 |
| Vulkan featureの通常試験 | CPU単体107件＋公開API 3件成功、26 ignored |
| presentation featureの通常試験 | CPU単体111件＋公開API 3件成功、27 ignored |
| 明示headless実GPU試験 | 25件成功、2.71秒 |
| headless＋実Win32 Surfaceを含む明示GPU試験 | 26件成功、4.57秒。WSIではVSync切り替えも確認 |
| Validation / SyncVal | 実効設定を確認し、native所有者の破棄後までwarning・error・callback失敗ゼロ |
| Shader再生成検証 | 5種類すべてspirv-val成功、固定SDKによる再生成とチェックインbinaryのSHA-256一致 |
| headless例 | 従来の128×96描画・深度・Compute・合成とPPM出力成功 |
| engine headless統合例 | 24 frame、3 in-flight、custom処理・history・更新/削除・遅延readbackの照合成功 |
| engine Windows表示例 | ON/OFF各起動の`windowed --smoke`で12 Present要求、VSync往復切り替え・resize・最小化復帰・pending終了、終了コード0 |
| 表示例の引数検査 | VSync既定ON、ON/OFF指定、smoke併用、不正引数拒否のCPU試験1件成功 |
| JSON出力のCPU試験 | Unicode/control文字escape、中央値とnearest-rank p95の2件成功 |
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
| import/export | pending producerをCPU waitせずconsumerへ引継ぎ、owner/generation/revision、read-onlyでも進むrevision、fresh import、順不同wait、削除と再利用 |
| frame進行 | metadata Dropは非待機、Runtimeのbounded pending、投入前capacity拒否、単件poll、timeout保持、明示drain、native完了とreadbackのCPU寿命の分離 |
| 再利用 | 3 in-flight×4 roundで未読24 readback leaseを保持して再利用、warm-up後の作成数はresource 9 / execution 3 / descriptor arena 3で安定、結果byte不変 |
| 容量・削除 | pending中の削除と旧key拒否、別entry/世代、size切替、4096-byte readback容量の不足と結果消費後の回復、既存の未投入失敗回収 |
| history | 18 frameの連鎖＋read-only利用、初回・同extent reset・5×3へのresize、3件連続のhost未待機submit、pending consumer中reset、CPU参照との全byte照合 |
| engine入力 | owner/世代付きmesh/material CRUD、immutable snapshot、旧snapshotの独立利用、View/model/tintの画素変化、empty draw、実index上限、GPU pending中の更新・削除 |
| 準備と投入 | 二重prepare、build後CPU更新、旧uploadを含むsubmit、foreign Graph事前拒否、現在版mesh cacheを旧snapshotで置換しない |
| custom pass | crate外のconsumerによる標準Graphへの処理追加、同寸linear RGBA8の契約、foreign handle/format/extent/未初期化の拒否、不要branchの除去 |
| 計測query | 通常off、pending lease保持、disable後の受理済みsample回収、完了後即再利用、readbackの遅延消費、有効bit幅・wrap・availability |
| WSI | 実Win32 Surfaceで18表示、各frameのheadless画素照合、resize/連続resize、ゼロextent/最小化復帰、取得後cancel、pending close、Windowの保持と解放 |
| 失敗状態 | CPU境界試験でsubmit OOMの状態不変、timeout/不確実失敗の保持、Device loss、presentのenqueue区分、SUBOPTIMAL、上限なし画像数等を確認 |

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

## 第二APIの選定と未確認範囲

M4の第二APIはD3D12を選定した。既存Windows実機とshaderツールを使って、
同じ入力・Graph・GPUでVulkanと比較しやすいことが理由である。
D3D12CreateDevice(default adapter, FL11_0)とID3D12Debug取得はS_OK、直後にreleaseし命令投入なし。
adapterの識別、Debug Layer/GBV有効化、実描画と画像等価性、実GPU CIは未確認。
準備は`renderer-6qc`、`renderer-k51`に登録し、M3で第二backend実装や一般RHIを先行固定していない。

別GPU/driver、Linux表示・実GPU、Mac/Metal、実native OOM/Device loss/Surface loss、
拡張なしWSI、透明Window、HDR、物理aliasing、複数queueは今回の追加実機確認に含まない。
clock・affinity・電力・OS負荷は固定せず、他エンジンとの性能比較も行っていない。
過去のtransfer-only Criterionや局所compiler比較の数値を、現在のframe基準値やFPSに読み替えない。

## 再現物

最終ゲートは`.tools/m3/gates/{cpu,vulkan,presentation}-{check,clippy,test}.log`、
`gpu-all.log`、`windowed-smoke.log`、`example-cpu.log`、`shader-verify.log`、`headless-example.log`。
Linux CI修正後の追加確認は`ci-fix-{vulkan,presentation}-{check,clippy,test}.log`と`ci-fix-gpu.log`。
統合verifyのJSON/PPMは`.tools/m3/frame-verify.json`、`frame.ppm`、
WSI能力の元情報は`.tools/m3/preflight-vulkaninfo.log`、第二API probeは`.tools/m3/second-api-probe.txt`。
`.tools/`はGit対象外で、再実行方法は開発ガイドに記載する。
性能runの生ログは`.tools/m3/gates/frame-baseline.log`、Gitで共有する基準データは
[current-metrics.json](current-metrics.json)を正とする。

# 現行実装の検証状況

最終確認日: 2026-11-12。実行手順は[開発ガイド](../guides/development.md)、
ツールの使い方は[ツールガイド](../guides/tooling.md)、
保証する契約は[Vulkanの設計判断](../design/vulkan.md)を参照する。
ここには現行実装に対する最新の確認を記録し、過去版の記録はGit履歴で参照する。

## RenderGraph設計ガイドの調査・文書確認

2026-11-12、`renderer-sc3`で[RenderGraphベストプラクティス](../guides/rendergraph-best-practices.md)を追加した。
Frostbite、Unreal、Unity、Filament、O3DE、RPS、Falcorの一次資料・固定版ソースと、
Vulkan／VMA／D3D12／Metalの公式資料を照合した。API契約、一般推奨、設計上の選択、
条件付き最適化、資料からの整理・推論を区別し、現行Graphの設計とは独立したガイドにした。
文書レビュー、参照定義・相対リンク・目次アンカー・UTF-8と差分の空白検査を実施した。
この変更は文書のみで、Rustのビルド・CPU／実GPU試験、比較対象エンジンの実行、
GPU性能の比較測定は追加実行していない。以下の実装検証結果を更新するものではない。

## 環境

| 項目 | 確認値 |
|---|---|
| OS / target | Windows x64 / `x86_64-pc-windows-msvc` |
| CPU | Intel Core i5-14600KF |
| Rust / Cargo | 1.99.0 / 1.99.0 |
| GPU / driver | NVIDIA GeForce RTX 3060 / NVIDIA 610.88 |
| Vulkan SDK / Loader | 1.4.341.1 / 1.4.341 |
| Instance / VMA要求API | Vulkan 1.3 |
| Queue | family 0、index 0、Graphics＋Compute 1本 |

## 正しさと品質確認

| 確認 | 結果 |
|---|---|
| fmt、CPUのみ／Vulkanの全target check・Clippy `-D warnings` | 成功 |
| CPUのみの通常試験 | 単体14件＋公開API 2件成功、Vulkan初期化なし |
| Vulkan feature付き通常試験 | CPU単体77件＋公開API 2件成功、明示実行用16件はignored |
| 実Vulkan正しさ試験 | 15件成功、1.94秒。既存9件＋Graph 6件 |
| batchの全byte照合 | 2回の生成・終了で計56回、うち順不同の部分転送32回 |
| Validation warning / error、callback内部失敗 | ゼロ |
| Shader再生成一致 | 5種すべて`spirv-val`成功、チェックイン済みbinaryとSHA-256一致 |
| ヘッドレス描画例 | 128×96のPPM生成成功、中央の黄色と背景の白を確認 |
| Criterionの測定なしの実行確認 | 全24 case成功 |
| 名前付き基準値との比較 | offlineで成功、保存済み基準の内容を保持 |
| GitHub Actions | [Windows／Linuxの品質確認と依存監査の実行結果](https://github.com/tsukushibito/renderer/actions/workflows/ci.yml)を参照 |

実GPU試験ではValidationとSyncValの実効設定を確認した。
半開区間の境界、初期化範囲の穴、範囲統合、各追加順、RAW／WAR／WAW、
read同士、同じcopy内の照合順、global依存後の追跡更新をCPUで確認した。
GPUでは順不同の隙間付きupload、未初期化の隙間の拒否、各範囲のreadbackを照合した。
実GPUの内容照合と、CPUだけで行う範囲管理の試験・性能測定は区別する。

## RenderGraphの実行確認

[Graphの契約](../design/rendergraph.md)に従う宣言・解析・実行を追加した。
GPUなしで型付きハンドル、byte範囲別producer、初期化の穴、read_write、
Attachmentのclear／load／store／discard、循環、明示依存、出力・副作用の到達性を検査する。
上書き済みwriterと不要readerを除去しつつ、liveなRAW／WAR／WAWを保つ回帰試験、
同じ入力のdumpが一致する試験、公開APIだけで描画フローを構築する試験が成功した。

Graphの実GPU試験6件では次を確認した。

| 対象 | 実際に確認した動作 |
|---|---|
| Copyと非同期import/export | buffer／17×13画像のupload→Copy→readbackを全byte照合。A→Bを間のCPU waitなしで投入し、B→Aの順で待っても状態が巻き戻らない |
| 容量・owner・世代 | 実行枠・資源・staging不足、削除・古いsnapshot・別runtimeの拒否。未投入失敗後の回収と再実行、二重readback消費の拒否 |
| Compute | 17×13の端数dispatchを含むInvert→SwapRedBlueの依存を実行し、両画像の全byteを厳密照合。4回繰り返して画像／View・実行枠・descriptor arenaの再利用を確認 |
| indexed meshと深度 | 16×16の遠い赤い四角形と近い緑の四角形を両描画順で全画素厳密照合。別rendering scopeのcolor／depth LOAD、vertex／indexのCopy read後の入力stageも確認 |
| 完全なM2フロー | mesh＋depth→依存するCompute 2段→別画像へsampled composite→export／readbackを全画素照合。不要branchの除去、未完了Graphへの引き継ぎ、削除と再利用も確認 |
| 記録前の拒否と回収 | descriptor容量不足と、実際のupload・部分書き換えから得た範囲外indexをnative draw前に拒否。取得済みleaseを返し、次の正常Graphが成功 |

描画fixtureは整数pixel境界と0／1の色、Computeは0〜255の既知byteパターンを使い、
どちらも浮動小数点の許容誤差を設けずに厳密比較した。
すべてCore validation＋SyncValの実効設定と、破棄後までの診断を検査した。
入力拒否を確認するアプリケーションERRORログは、Validationの異常とは区別する。
pipelineはruntimeで保持し、再実行で画像／View、descriptor arena、実行枠の作成・再利用カウンターを確認した。
新たな速度改善やFPSを測った結果ではない。

最終の実行コマンドは次のとおり。通常の全target check・Clippy・testはCPUのみとVulkanの両構成で成功した。

```powershell
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --no-default-features
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo test --locked --workspace --no-default-features
cargo check --locked --workspace --all-targets --no-default-features --features vulkan
cargo clippy --locked --workspace --all-targets --no-default-features --features vulkan -- -D warnings
cargo test --locked --workspace --no-default-features --features vulkan
cargo test --locked -p renderer --no-default-features --features vulkan ::vulkan_ -- --ignored --nocapture --test-threads=1
& crates/renderer/shaders/regenerate.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
& ./.tools/cargo-deny/bin/cargo-deny.exe --locked --color never check
cargo run --locked -p renderer --features vulkan --example headless -- .tools/m2.ppm
```

生ログは`.tools/vulkan-research/m2-final-{cpu,vulkan,gpu,shaders,deny,example}.log`、
描画例は`.tools/m2.ppm`に保存した。Rustのincremental cacheでhard linkが使えないという
実行環境のwarningはあったが、ソースのClippy `-D warnings`は成功した。

## CPU転送計画の性能測定

Criterion 0.8.2で、8・128・1,024・8,192範囲の順方向・逆方向・固定seedの並べ替え順を測定した。
同じbufferに4-byte writeを16-byte間隔でN件計画し、その後N件のreadを計画する。
40 samples、500 ms warm-up、2秒のmeasurement目標を指定した。
入力生成、初期状態の構築、最後の所有者の破棄は測定外で、
範囲管理の検索・挿入・依存判定と、それに伴う割り当てや追跡集合の破棄を含む。
VMA、byte列のコピー、完全なBatchBuilder、submit、GPU実行は含まない。

以下は現行実装の8,192範囲における推定時間と95%信頼区間である。
中央値、個別転送のp95、描画FPSを表す値ではない。

| 追加順 | 推定時間（ms） | 95%信頼区間（ms） |
|---|---:|---:|
| 順方向 | 16.093 | 13.837–18.291 |
| 逆方向 | 9.710 | 8.105–11.296 |
| 並べ替え順 | 22.294 | 19.202–25.326 |

OSの負荷とthread affinityは固定しておらず、同じソースの実行間にも大きな揺れがあった。
実行内の信頼区間は、この実行間変動を解消しない。
小件数では順序付き集合の管理コストが増える場合があり、安定した改善倍率や損益分岐件数は確定していない。
範囲管理から二次時間の走査・要素移動を除いたことはソース構造で確認している。
通常のCIに壁時計時間の固定合否閾値は設けていない。

## 開発ツール

| ツール | 導入・確認結果 |
|---|---|
| cargo-deny 0.20.2 | 公式SHA-256に一致。Windows／Linux・全feature・開発用／ビルド用依存の4検査が成功 |
| SPIRV-Tools v2026.1 | 固定SDKの`spirv-val`、commit `fbe4f3ad`を確認 |
| shaderc v2026.1 | 一時GLSL ComputeをVulkan 1.3向けSPIR-Vへコンパイルして正常検証 |
| SPIR-V検証スクリプト | 正常入力2件と、不正binary・欠落入力・欠落tool・ディレクトリ入力の4件が期待どおり終了 |
| RenderDoc 1.47 x64 | 公式detached GPG署名が有効。CLI起動、DLLのロード、API 1.7.0取得に成功 |
| RenderDocのVulkan接続 | 子プロセスのmanifest追加パスで、instance／device両方へのcapture layer挿入を確認 |

cargo-denyのadvisory例外はない。Criterionの間接依存による`syn` 2／3の重複warningが1件あり、
理由と扱いを`deny.toml`へ記載している。実VulkanのValidation warning／errorゼロとは別の検査である。
SPIR-Vの正常検証は終了コード0、異常4件は終了コード1だった。
RenderDocでの実GPUキャプチャは未実行である。
portable版のregistry登録は行わず、子プロセス限定の`VK_ADD_IMPLICIT_LAYER_PATH`と
capture有効化指定を使った`vulkaninfo --summary`が終了コード0だった。
通常の利用は[ツールガイド](../guides/tooling.md)のRenderDoc起動方法に従う。

現在のWindows配布物の確認値は次のとおり。

| 配布物 | SHA-256 |
|---|---|
| cargo-deny 0.20.2 x64 MSVC | `975a22143262fd27476d19ee00c7af67978426e40e1dee94eed6bbade1cf87dc` |
| RenderDoc 1.47 x64 ZIP | `b51712e58d40066b34ca30c7869d15f72e4b5c001b2d92232f6783a636d5a3c1` |

RenderDocの署名鍵はfingerprint `1B039DB9A4718A2D699DE031AC612C3120C34695`に照合した。
registryへのVulkan layer登録、global hook、永続PATH、実行ポリシーの変更は行っていない。

## 確認の範囲

この記録の実行確認は上記Windows環境による。ローカルLinux、別CPU・別GPU、
実機non-coherentメモリ、native OOM・Device loss・reset失敗は未確認である。
CPU境界試験やソース監査の成功を、それらの実機確認とは扱わない。
描画・Compute・Graph間の非同期引き継ぎは上記の固定fixtureで確認済みである。
WSI、一般的な複数フレーム管理、未信頼shader、別API、完全なBatchBuilderやGPU転送全体の
新しい性能値は未確認・未実装であり、この結果から対応を主張しない。

## 実装指針の資料調査

2026-11-12にKhronos仕様・Guide・Samples、固定SDKのLunarG/VVL、VMA、shaderc／SPIRV-Tools、
GPUベンダーの一次資料を調べ、[Vulkanベストプラクティス](../guides/vulkan-best-practices.md)へ整理した。
仕様要件、一般推奨、機材に依存する最適化、本Rendererの方針を区別している。
VMAは`vk-mem 0.5.0`同梱の3.3.0 headerと公式の固定版ソースを照合した。

分野別の独立レビューで同期・寿命・descriptor・WSI等の記述を確認し、
文書の相対リンク・節アンカーと差分の空白検査を行った。
これは文書調査の確認であり、Rendererの実装変更・GPU試験・性能測定は行っていない。
上記の実行済み結果を再実行した記録や、後続機能の実機確認としては扱わない。

## ベストプラクティスの適用と追加レビュー

M2ではガイドの能力照会・VMA所有・同期・Shader ABI・binding・描画・検証を適用した。
独立レビューでGraph内容追跡とculling、資源同一性とGPU寿命、Shaderとdescriptor、
投入受理と失敗経路を確認し、見つかった問題を修正してから上記の最終検証を実行した。
単一キュー・全体buffer barrier・固定usage／Shaderは保守的な基準で、
vendor依存最適化や未測定の性能改善を採用したものではない。

追加レビューでは、既存の転送bufferにも`maxBufferSize`の事前検査不足が見つかった。
前回の「監査範囲で指摘なし」は網羅性の保証ではなく、この条件は拾えていなかった。
Vulkan 1.3のpropertyを一度照会し、転送・shared staging・Graphの全buffer経路で
VMA呼び出し前に上限超過を拒否するよう修正した。RTX 3060の上限1 TiBに対する
上限＋1の拒否と、zero／上限／上限＋1のCPU境界試験が成功した。

新規Graph経路では、用途の変わるread間でwriter可視性を保つこと、
submit後のログpanicでもrevisionを再利用しないこと、export登録guard、
不確実なsubmit失敗時の停止、型付きエラーと元の原因の保持を確認した。
未投入OOM・未知submit結果・timeout・lossのCPU境界試験は実際のdriver障害の誘発ではない。
現時点で未解決のレビュー指摘はないが、実機non-coherentメモリ、native OOM／Device loss／
reset失敗、別GPU・ローカルLinuxでの確認範囲は拡大していない。

生ログと配布物はGit対象外の`.tools/`、Criterionの保存値は`target/criterion/`にある。
過去の完全な比較表が必要な場合は、たとえば次のようにGit履歴から参照する。

```sh
git show a032edf:docs/transfer-planning-baseline.md
```

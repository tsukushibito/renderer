# 転送計画の範囲検索・Criterionの確認結果

- 確認日: 2026-11-12
- 関連課題: `renderer-bp9`、`renderer-muo`
- 判断と契約: [ADR 0009](adr/0009-ordered-transfer-planning.md)
- 再現手順: [開発手順](development.md#cpu転送計画の性能測定criterion)

## 実装と測定範囲

同期判定の既往アクセス走査と、初期化済み範囲のVecによる検索・挿入を、
共通の`ByteRanges`へ置き換えた。半開区間を`BTreeMap`で保持し、重複・隣接範囲を統合する。
包含・重なり検索はO(log K)、N件の挿入列は償却O(N log N)である。
独立した範囲を追加し続ける場合の二次時間の走査・要素移動を解消した。
read同士と独立範囲の並行性、RAW／WAR／WAWの依存、実Fence完了後の内容確定を維持する。

CPU benchmarkは同じbufferへN件の4-byte writeを16-byte間隔で計画し、その後N件のreadを計画する。
順方向、逆方向、固定seedの並べ替え順を、8・128・1,024・8,192件で測定した。
旧Vecによる同期判定・初期化範囲管理の比較用実装と、本番で使う現行ソースを同じ実行内で比較した。
入力生成、初期状態の構築、最終所有者の破棄は測定区間から外す。
範囲の検索・挿入・依存判定と、それに伴う割り当てや追跡集合の破棄を含む。
表示上のthroughputは2N件のメタデータアクセスである。
完全なBatchBuilder、VMA、byte列のコピー、submit、GPU実行時間の測定は含まない。

## 環境と設定

| 項目 | 確認値 |
|---|---|
| OS / target | Windows x64 / `x86_64-pc-windows-msvc` |
| CPU | Intel Core i5-14600KF |
| Rust / Cargo | 1.99.0 / 1.99.0 |
| Criterion | 0.8.2 |
| 今回の測定設定 | 40 samples、500 ms warm-up、2秒のmeasurement目標 |
| CPU benchmark中のVulkan初期化 | なし |

measurementは目標時間であり、必要なsample数を収集するため2秒を超えるcaseもある。
これはソース既定の30 samples・250 ms・1秒をCLIで上書きした測定である。
同じ設定を指定する再現例は次のとおり。

```sh
cargo bench --locked -p renderer --no-default-features --bench transfer_planning -- --save-baseline ordered-ranges-final --sample-size 40 --warm-up-time 0.5 --measurement-time 2
```

Criterionの開発用依存allocaがC helperをビルドするため、Cコンパイラーが必要である。
通常のCPUライブラリビルドの依存は変わらず、Vulkan SDKやVMAのC++ビルドはこのbenchmarkに不要である。
測定データは`.cargo/config.toml`の`CRITERION_HOME`既定値によりworkspaceの`target/criterion/`へ保存する。
既存環境変数で上書きでき、`CARGO_TARGET_DIR`とは独立して扱う。
名前付き基準値との比較も確認し、`--baseline`が保存済み基準を上書きしないことを確認した。

## 測定結果

値はCriterionの推定時間と95%信頼区間で、単位はすべてµsである。
各行は24 caseを追加順・件数ごとに対にしたもの。中央値や個々の転送のp95ではない。

| 追加順 | N | 旧Vec 推定 [95% CI]（µs） | 現行 推定 [95% CI]（µs） |
|---|---:|---:|---:|
| 順方向 | 8 | 0.535 [0.527, 0.547] | 6.152 [5.341, 6.827] |
| 順方向 | 128 | 122.380 [113.607, 128.627] | 174.430 [148.838, 195.678] |
| 順方向 | 1,024 | 3447.097 [2738.083, 4060.224] | 2484.392 [2352.319, 2587.054] |
| 順方向 | 8,192 | 188213.945 [157745.025, 218372.155] | 16093.239 [13837.175, 18291.352] |
| 逆方向 | 8 | 4.572 [3.933, 5.153] | 5.606 [4.432, 6.819] |
| 逆方向 | 128 | 109.365 [91.374, 125.908] | 112.133 [90.588, 136.566] |
| 逆方向 | 1,024 | 3799.733 [3224.584, 4353.204] | 756.929 [606.600, 909.355] |
| 逆方向 | 8,192 | 209796.075 [177934.910, 241656.472] | 9709.888 [8105.072, 11296.058] |
| 並べ替え順 | 8 | 3.147 [2.481, 3.798] | 4.710 [3.623, 6.022] |
| 並べ替え順 | 128 | 109.352 [90.780, 123.327] | 147.486 [118.619, 171.772] |
| 並べ替え順 | 1,024 | 6050.728 [5477.120, 6555.497] | 2461.033 [2025.843, 2840.068] |
| 並べ替え順 | 8,192 | 542994.285 [515704.653, 567096.520] | 22293.866 [19202.412, 25325.587] |

この実行では8,192件の現行方式が旧方式より短かった。
主な結論は、ソース上で二次時間の検索・挿入を除いたことである。
この計測値だけから正確な改善倍率や描画FPSを一般化しない。

## 揺れと解釈の制限

同じソースの測定間にも大きな揺れがあった。
順方向8件の旧方式は初回約4.80 µs、上表の最終実行では約0.535 µsだった。
順方向128件の現行方式は基準値CLI確認時に約20.15 µs、最終実行では約174.43 µsだった。
OSの負荷とthread affinityは制御しておらず、実行内の信頼区間ではこの実行間変動を解消できない。

小件数では順序付き集合の管理コストが増える場合がある。
小件数の性能劣化がないとは判断せず、正確な劣化倍率や損益分岐件数もこの結果から固定しない。
今回の証拠では切替閾値を決められないため、Vecとのhybrid構成は追加していない。
通常のCIに壁時計時間の固定合否閾値も設けていない。

## 正しさと品質確認

| 確認 | 結果 |
|---|---|
| fmt、CPUのみ／Vulkanの全target check・Clippy `-D warnings` | 成功 |
| CPUのみの通常試験 | 0件、Vulkan初期化なし |
| Vulkan feature付き通常試験 | CPU 53件成功、明示実行用10件はignored |
| Criterionの全caseを測定なしで確認 | 24 case成功 |
| 実Vulkan正しさ試験 | 9件成功、1.21秒 |
| batchの全byte照合 | 2回の生成・終了で計56回、うち隙間を持つ部分転送の追加確認32回 |
| Validation warning / error、callback内部失敗 | ゼロ |

実Vulkan試験はNVIDIA GeForce RTX 3060で行い、ValidationとSyncValの実効設定を確認した。
新しいCPU範囲集合では境界、穴、統合、並べ替え順を、同期判定ではread同士とRAW／WAR／WAWを確認した。
GPU試験は範囲集合を使うバッチの転送内容と同期を確認するもので、GPU性能測定ではない。

## 証拠と未検証範囲

Git対象外の`.tools/planning-criterion-final.log`と`.tools/planning-criterion-summary.json`に、
最終実行の出力と推定値・信頼区間を保存した。
測定間の揺れは`.tools/planning-criterion.log`と`.tools/planning-criterion-baseline-check.log`でも確認できる。
品質確認と実Vulkanの出力は`.tools/planning-*-check.log`、`.tools/planning-*-clippy.log`、
`.tools/planning-*-test.log`、`.tools/planning-vulkan.log`に記録した。
生ログの共有を前提とせず、条件と全caseの値を本書へ記録する。

今回の変更について、ローカルLinux実行と新しいCI実行は未確認である。
別CPU・別GPU、実機non-coherentメモリ、native OOM・Device loss・reset失敗の追加確認は行っていない。
完全なBatchBuilderやGPU転送全体の新しい性能値、描画・複数フレームの性能は測定していない。

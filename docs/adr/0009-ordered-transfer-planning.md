# ADR 0009: 転送計画の範囲検索とCriterionによる性能比較

- 日付: 2026-11-12
- 状態: 採用
- 関連課題: `renderer-bp9`、`renderer-muo`

## 背景

バッチの同期判定はbufferごとの既往アクセスをVecに保持し、各アクセスで線形走査していた。
同じbufferへの重ならない部分転送がN件続くと、比較回数がN(N−1)/2になる。
初期化済み範囲もVecを使っており、検索と逆順・並べ替え順の挿入に同じ増加が残る。
GPU完了後の初期化範囲確定でも挿入を使うため、同期判定だけの置き換えでは不十分である。
ユーザーはこの増加を修正必須とし、Criterionの導入を選択した。

## 採用する構成

backend内部の小さな`ByteRanges`を、初期化済み範囲と同期判定で共用する。
標準ライブラリの`BTreeMap`で半開区間を開始位置の順に保持し、
重複する範囲と隣接する範囲を統合する。独自の木構造、unsafe、追加の範囲管理依存は導入しない。

範囲全体の包含と重なりは直前の区間を検索して判定する。
K個の保持区間に対する検索はO(log K)である。
挿入で統合する区間は集合から除くため、挿入列全体は償却O(N log N)となる。
順方向だけでなく、隙間のある逆順・並べ替え順の追加でも二次時間の走査・要素移動を避ける。
初期化済み範囲では空範囲を包含すると扱い、空範囲の追加は何もしない。

同期判定はbufferごとにreadとwriteの集合を分ける。
新しいreadは既往write、新しいwriteは既往readとwriteに対して重なりを確認する。
同じcopyに含まれる全アクセスを確認してから集合へ追加する。
依存が必要になったときはglobal barrierがそれまでのアクセスを順序づけるため、
全bufferの追跡集合をクリアする。read同士と独立した範囲には追加のbarrierを要求しない。

GPU資源の所有、実Fence完了後の内容確定、共有staging領域の回収は
[ADR 0008](0008-pre-m2-execution-reuse.md)の契約を維持する。
この集合はCOPY専用の範囲判定であり、shader用途やGraphの依存解析を実装するものではない。

## 性能測定

Criterion 0.8を開発用依存へ追加し、`harness = false`の専用`transfer_planning` targetを設ける。
本番の範囲集合と同期判定のソースを組み込み、内部APIの公開範囲を広げずにCPUだけで測定する。
比較用の旧Vec実装はbenchmark内に保持する。
採用版Criterion 0.8.2はWindows／Unixでalloca 0.4を利用し、
alloca 0.4.0のbuild scriptがC helperをコンパイルする。
そのためCPUのテスト・benchmarkにもCコンパイラーが必要となる。
通常のCPUライブラリビルドの依存は変えず、Vulkan SDKやVMAのC++ビルドはCPU benchmarkに要求しない。

同一bufferの隙間を持つ範囲を、順方向・逆方向・固定seedの並べ替え順で追加する。
8・128・1,024・8,192件のwrite後に同件数のreadを計画し、
検索・挿入の費用と件数に対する増え方を確認する。
入力生成、初期状態の構築、測定後の所有者の破棄は計測区間から外す。
範囲の検索・挿入・依存判定と、それに伴う割り当てや追跡集合の破棄は測定に含める。
Criterionへウォームアップ、サンプリング、統計解析、保存した基準との比較を任せる。
機材や実行負荷による揺れがあるため、壁時計時間の固定閾値を通常のCIへ追加しない。

測定データの既定保存先はworkspaceの`target/criterion/`とする。
[Cargo設定](../../.cargo/config.toml)の`[env]`と`relative = true`で`CRITERION_HOME`を渡し、
CriterionがCargo metadataを取得できない場合も保存先を一定にする。
既存の`CRITERION_HOME`環境変数は優先し、個別の保存先を指定できる。
`CARGO_TARGET_DIR`によるビルド出力の変更とは独立して扱う。

CPUの推定時間と信頼区間を、個別転送のp95やGPU実行時間と同一視しない。
既存の明示実行用GPU計測は、Vulkan timestamp、実Fence完了、区間別時間、
個別転送のp95と内容照合を扱うため維持する。
Criterionを導入しても、GPU資源の保持・完了確認・正誤判定は実装側の責任である。

## 比較した案と影響

Vecの隣接範囲だけをまとめる案では、隙間のある逆順・並べ替え順の最悪ケースを解消できない。
独自のinterval treeは今回必要な包含・重なり・統合に対して実装と検証を増やすため採らない。
標準の順序付き集合へ寄せ、小件数の定数コストも含めて測定する。

初期化範囲とread／write集合で同じ小さな実装を使い、重複した範囲アルゴリズムを増やさない。
VMAは引き続き物理メモリと共有buffer内の領域配置を担当する。
`ByteRanges`は初期化・アクセス状態の集合であり、空きメモリを配置するallocatorではない。

## 検証と見直し

CPU試験では半開区間の境界、穴をまたぐ包含、隣接・重複の統合、
RAW／WAR／WAW、read同士、同一copyの照合順、global依存後の追跡更新を確認する。
実Vulkanのバッチ試験では転送内容とValidation／SyncValを確認する。
性能値は実行機材・設定・計測範囲と共に記録し、CPU範囲管理の改善を描画FPSの改善とは扱わない。
手順は[開発環境](../development.md)を参照する。
実測値と測定間の揺れ、検証状況は[転送計画の確認結果](../transfer-planning-baseline.md)に記録する。

## 一次資料

- [標準ライブラリのBTreeMap](https://doc.rust-lang.org/std/collections/struct.BTreeMap.html)
- [Criterionの計測ループ](https://docs.rs/criterion/0.8.2/criterion/struct.Bencher.html)
- [Criterionの設定](https://docs.rs/criterion/0.8.2/criterion/struct.Criterion.html)
- [CriterionのCLI・名前付き基準値](https://criterion-rs.github.io/book/user_guide/command_line_options.html)
- [採用alloca 0.4.0のC helperビルド](https://docs.rs/crate/alloca/0.4.0/source/build.rs)
- [Cargo設定の環境変数と相対パス](https://doc.rust-lang.org/cargo/reference/config.html#env)

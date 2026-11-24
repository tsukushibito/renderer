# 研究施設の床と鋼板

床には Concrete Floor 02、実験設備には Blue Metal Plate を使う。いずれも Rob Tuytel の素材で、Poly Haven が CC0 で配布している。約2 m 四方を収めた床材には、見下ろし視点で縮小しても残る摩耗やしみがある。[床の配布元](https://polyhaven.com/a/concrete_floor_02)・[鋼板の配布元](https://polyhaven.com/a/blue_metal_plate)・[ライセンス](https://polyhaven.com/license)

ゲームは同梱の RGBA8 画像を直接使う。各ファイルは 256 × 256 から 1 × 1 までの9段を大きい順に詰めたもの。ベースカラーには sRGB、OpenGL 法線と ORM（遮蔽・粗さ・金属度）には線形形式を指定する。床の色は線形光で平均し、法線はフィルタリング後に単位長へ戻している。

原本の取得先・SHA-256 は [sources.json](sources.json) に固定した。リポジトリのルートで次を実行すると、初回は6枚の 1K PNG を取得し、次回からは検証済みのローカルコピーを使う。再生成結果は6ファイルすべてバイト比較される。

```powershell
python -m pip install numpy==1.26.4 Pillow==11.1.0
python -B tools/survivor/cook_environment.py --check
```

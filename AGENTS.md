# 開発方針

- アプリ名は `MotocalGUI`、パッケージ／実行ファイル名は `motocalgui`。SpaceCal for Monado から独立した hard fork として扱い、配布先は `ReinaS-64892/MotocalGUI` とする。原作者の著作権表示と謝辞は保持する。

- このアプリは libmotoc の UI として実装する。校正・リセンターなどは可能な限り libmotoc の公開 API に任せ、独自の計算や補正処理を追加しない。
- Monado API を直接操作する必要がある場合は、可能な限り libmonado を使用する。独自の FFI や API バインディングは実装しない。
- libmotoc の API だけでは対応できない部分は、motoc CLI の実装を参照し、その動作に合わせる。現在、libmonado への直接書き込みを許容するのは明示的なリセット、保存済み校正の再適用、および motoc tui に合わせた明示的な Adjust 操作のみ。それ以外の書き込みは libmotoc に任せる。
- リセットは motoc CLI と同様に、指定されたトラッキングオリジンまたは STAGE／LOCAL 参照空間へ恒等変換を書き込む。
- 保存済み校正の復元も motoc CLI に合わせる。通常の校正は保存オフセットと現在のソースオリジンのオフセットを CLI と同じ順序で合成して適用し、継続校正は libmotoc の処理を使用する。
- 校正データの読み書きは libmotoc に任せる。motoc CLI と同じ既定プロファイル `last`、保存先 `$XDG_CONFIG_HOME/motoc/last.json`（通常 `~/.config/motoc/last.json`）、データ形式を共有する。独自形式での保存や、アプリ側での校正 JSON の直接読み書きは行わない。
- motoc に存在しない UI 固有設定は別に保存する。現在の保存先は `$XDG_DATA_HOME/motocalgui/config.json`（通常 `~/.local/share/motocalgui/config.json`）。共有校正データへ UI 固有フィールドを追加しない。
- libmotoc にない機能を独自に維持するより、機能や UI を省くか libmotoc の動作に合わせる。

# コミット方法

コミットを作成する際は、必ず以下のコマンド形式を使用する。リポジトリやグローバルの Git 設定は変更しない。

```bash
git -c user.name='OpenAI Codex' -c user.email='codex@openai.com' -c commit.gpgSign=false commit --author='OpenAI Codex <codex@openai.com>' -m 'コミットメッセージ'
```

この方針の記載自体は、コミットを作成する指示ではない。コミットはユーザーから依頼された場合に作成する。

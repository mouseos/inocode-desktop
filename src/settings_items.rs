//! The Codex configuration keys the settings page edits, grouped by category.
//! Keys are the config.toml paths (dotted); values are read from `config/read` and written with
//! `config/value/write`. The list comes from codex-rs/core/config.schema.json
//! (see ref/docs/17-settings-coverage.md for what is and is not covered).

pub(crate) enum K {
    Bool,
    /// (shown, written)
    Choice(&'static [(&'static str, &'static str)]),
    Int,
    Text,
    /// A list of strings, typed as a space-separated line.
    List,
}

pub(crate) struct Item {
    /// Index into CATEGORIES.
    pub cat: usize,
    pub key: &'static str,
    pub label: &'static str,
    pub kind: K,
}

/// Settings sections that are driven by this table, in nav order. The other sections
/// (shortcuts, memory, InoCode, account, ...) are listed in settings_page.rs.
pub(crate) const CATEGORIES: [&str; 6] =
    ["一般", "承認とサンドボックス", "コンテキストと履歴", "通知とプライバシー", "表示", "エージェントとメモリ"];

const LOW_MED_HIGH: &[(&str, &str)] = &[("簡潔", "low"), ("標準", "medium"), ("詳細", "high")];
const EFFORT: &[(&str, &str)] = &[("最小", "minimal"), ("軽", "low"), ("中", "medium"), ("高", "high"), ("極高", "xhigh")];
const AUTO_ALWAYS_NEVER: &[(&str, &str)] = &[("自動", "auto"), ("常に", "always"), ("しない", "never")];

macro_rules! item {
    ($cat:expr, $key:expr, $label:expr, $kind:expr) => {
        Item { cat: $cat, key: $key, label: $label, kind: $kind }
    };
}

pub(crate) const ITEMS: &[Item] = &[
    // 0 一般
    item!(0, "model", "モデル", K::Text),
    item!(0, "model_provider", "モデルプロバイダ", K::Text),
    item!(0, "profile", "プロファイル", K::Text),
    item!(0, "model_reasoning_effort", "推論の強さ", K::Choice(EFFORT)),
    item!(0, "plan_mode_reasoning_effort", "プランモードの推論の強さ", K::Choice(EFFORT)),
    item!(0, "model_reasoning_summary", "推論の要約", K::Choice(&[("自動", "auto"), ("簡潔", "concise"), ("詳細", "detailed"), ("なし", "none")])),
    item!(0, "model_verbosity", "応答の詳しさ", K::Choice(LOW_MED_HIGH)),
    item!(0, "service_tier", "サービスティア", K::Text),
    item!(0, "review_model", "レビューのモデル", K::Text),
    item!(0, "personality", "パーソナリティ", K::Choice(&[("なし", "none"), ("フレンドリー", "friendly"), ("実務的", "pragmatic")])),
    item!(0, "web_search", "ウェブ検索", K::Choice(&[("無効", "disabled"), ("キャッシュ", "cached"), ("インデックス", "indexed"), ("ライブ", "live")])),
    item!(0, "file_opener", "ファイルを開くアプリ", K::Choice(&[("VS Code", "vscode"), ("VS Code Insiders", "vscode-insiders"), ("Windsurf", "windsurf"), ("Cursor", "cursor"), ("なし", "none")])),
    item!(0, "hide_agent_reasoning", "推論を隠す", K::Bool),
    item!(0, "show_raw_agent_reasoning", "生の推論を表示", K::Bool),
    item!(0, "daybreak", "Daybreak を既定にする", K::Bool),
    item!(0, "openai_base_url", "OpenAI の base URL", K::Text),
    item!(0, "chatgpt_base_url", "ChatGPT の base URL", K::Text),
    item!(0, "oss_provider", "ローカルモデルのプロバイダ", K::Text),
    item!(0, "model_catalog_json", "モデルカタログ(JSON のパス)", K::Text),
    // 1 承認とサンドボックス
    item!(1, "approval_policy", "承認ポリシー", K::Choice(&[("確認する", "untrusted"), ("要求時のみ", "on-request"), ("確認しない", "never")])),
    item!(1, "approvals_reviewer", "承認の審査者", K::Choice(&[("ユーザー", "user"), ("自動審査", "auto_review"), ("ガーディアン", "guardian_subagent")])),
    item!(1, "sandbox_mode", "サンドボックス", K::Choice(&[("読み取り専用", "read-only"), ("ワークスペース書き込み", "workspace-write"), ("フルアクセス", "danger-full-access")])),
    item!(1, "sandbox_workspace_write.network_access", "ワークスペース書き込み時のネットワーク", K::Bool),
    item!(1, "sandbox_workspace_write.exclude_tmpdir_env_var", "TMPDIR を書き込み対象から外す", K::Bool),
    item!(1, "sandbox_workspace_write.exclude_slash_tmp", "/tmp を書き込み対象から外す", K::Bool),
    item!(1, "sandbox_workspace_write.writable_roots", "書き込みを許す追加のフォルダ", K::List),
    item!(1, "windows.sandbox", "Windows のサンドボックス", K::Choice(&[("昇格", "elevated"), ("昇格なし", "unelevated"), ("mxc", "mxc")])),
    item!(1, "default_permissions", "既定の権限プロファイル", K::Text),
    item!(1, "allow_login_shell", "ログインシェルを許す", K::Bool),
    item!(1, "shell_environment_policy.inherit", "シェルの環境変数の継承", K::Choice(&[("コアのみ", "core"), ("すべて", "all"), ("なし", "none")])),
    item!(1, "shell_environment_policy.ignore_default_excludes", "既定の除外を無視", K::Bool),
    item!(1, "shell_environment_policy.include_only", "引き継ぐ環境変数(パターン)", K::List),
    item!(1, "shell_environment_policy.exclude", "除外する環境変数(パターン)", K::List),
    item!(1, "auto_review.policy", "自動審査のポリシー文", K::Text),
    item!(1, "auto_review.timeout_ms", "自動審査のタイムアウト(ms)", K::Int),
    // 2 コンテキストと履歴
    item!(2, "model_context_window", "コンテキストウィンドウ(トークン)", K::Int),
    item!(2, "model_auto_compact_token_limit", "自動圧縮のしきい値(トークン)", K::Int),
    item!(2, "model_auto_compact_token_limit_scope", "自動圧縮の対象", K::Choice(&[("全体", "total"), ("先頭以降", "body_after_prefix")])),
    item!(2, "model_post_turn_compact_threshold_percent", "ターン後の圧縮しきい値(%)", K::Int),
    item!(2, "tool_output_token_limit", "ツール出力の上限(トークン)", K::Int),
    item!(2, "project_doc_max_bytes", "プロジェクト指示の上限(バイト)", K::Int),
    item!(2, "project_doc_fallback_filenames", "AGENTS.md の代替ファイル名", K::List),
    item!(2, "project_root_markers", "プロジェクトルートの目印", K::List),
    item!(2, "history.persistence", "履歴の保存", K::Choice(&[("すべて保存", "save-all"), ("保存しない", "none")])),
    item!(2, "history.max_bytes", "履歴の上限(バイト)", K::Int),
    item!(2, "include_environment_context", "環境コンテキストを含める", K::Bool),
    item!(2, "include_environment_context_time", "環境コンテキストに日時を含める", K::Bool),
    item!(2, "include_permissions_instructions", "権限の説明を含める", K::Bool),
    item!(2, "include_collaboration_mode_instructions", "協調モードの説明を含める", K::Bool),
    item!(2, "include_apps_instructions", "アプリの説明を含める", K::Bool),
    item!(2, "instructions", "システム指示", K::Text),
    item!(2, "developer_instructions", "開発者指示", K::Text),
    item!(2, "compact_prompt", "圧縮のプロンプト", K::Text),
    item!(2, "model_instructions_file", "モデル指示ファイル", K::Text),
    // 3 通知とプライバシー
    item!(3, "tui.notifications", "通知", K::Bool),
    item!(3, "tui.notification_method", "通知の方式", K::Choice(&[("自動", "auto"), ("OSC 9", "osc9"), ("OSC 777", "osc777"), ("ベル", "bel")])),
    item!(3, "tui.notification_condition", "通知の条件", K::Choice(&[("フォーカスが無いとき", "unfocused"), ("常に", "always")])),
    item!(3, "notify", "通知フック(コマンド)", K::List),
    item!(3, "analytics.enabled", "利用状況の分析", K::Bool),
    item!(3, "feedback.enabled", "フィードバックの収集", K::Bool),
    item!(3, "check_for_update_on_startup", "起動時に更新を確認", K::Bool),
    item!(3, "suppress_unstable_features_warning", "開発中の機能の警告を隠す", K::Bool),
    item!(3, "log_dir", "ログのフォルダ", K::Text),
    // 4 表示
    item!(4, "tui.animations", "アニメーション", K::Bool),
    item!(4, "tui.show_tooltips", "ヒントを表示", K::Bool),
    item!(4, "tui.raw_output_mode", "生の出力モード", K::Bool),
    item!(4, "tui.session_picker_view", "セッション一覧の密度", K::Choice(&[("ゆったり", "comfortable"), ("密", "dense")])),
    item!(4, "tui.resume_cwd", "再開時のフォルダ", K::Choice(&[("現在", "current"), ("セッション", "session")])),
    item!(4, "tui.copy_on_select", "選択でコピー", K::Choice(AUTO_ALWAYS_NEVER)),
    item!(4, "tui.alternate_screen", "代替画面", K::Choice(AUTO_ALWAYS_NEVER)),
    item!(4, "tui.fullscreen_transcript", "全画面のトランスクリプト", K::Bool),
    item!(4, "tui.auto_recap", "自動の要約", K::Bool),
    item!(4, "tui.mouse_scroll_speed", "マウススクロールの速さ", K::Int),
    item!(4, "tui.rendering.tables", "表の描画", K::Bool),
    item!(4, "tui.rendering.math", "数式の描画", K::Bool),
    item!(4, "tui.rendering.mermaid", "Mermaid の描画", K::Bool),
    item!(4, "tui.rendering.lists", "リストの描画", K::Bool),
    // 5 エージェントとメモリ
    item!(5, "agents.enabled", "サブエージェント", K::Bool),
    item!(5, "agents.max_concurrent_threads_per_session", "同時に動かすスレッド数", K::Int),
    item!(5, "agents.max_depth", "サブエージェントの深さ", K::Int),
    item!(5, "agents.default_subagent_model", "サブエージェントの既定モデル", K::Text),
    item!(5, "agents.interrupt_message", "割り込みメッセージ", K::Bool),
    item!(5, "goals.max_goal_token_budget", "ゴールのトークン予算", K::Int),
    item!(5, "memories.generate_memories", "メモリを生成", K::Bool),
    item!(5, "memories.use_memories", "メモリを使う", K::Bool),
    item!(5, "memories.extract_model", "メモリ抽出のモデル", K::Text),
    item!(5, "memories.consolidation_model", "メモリ統合のモデル", K::Text),
    item!(5, "memories.max_rollout_age_days", "メモリ対象の最大日数", K::Int),
    item!(5, "memories.max_unused_days", "未使用メモリの保持日数", K::Int),
    item!(5, "mcp_oauth_credentials_store", "MCP OAuth の保存先", K::Choice(&[("自動", "auto"), ("ファイル", "file"), ("キーリング", "keyring")])),
    item!(5, "mcp_oauth_callback_port", "MCP OAuth のコールバックポート", K::Int),
    item!(5, "mcp_optional_startup_grace_ms", "任意の MCP の起動待ち(ms)", K::Int),
    item!(5, "cli_auth_credentials_store", "認証情報の保存先", K::Choice(&[("ファイル", "file"), ("キーリング", "keyring"), ("自動", "auto"), ("保存しない", "ephemeral")])),
];

/// Value of a dotted key in the `config/read` result.
pub(crate) fn lookup<'a>(config: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let mut v = config;
    for part in key.split('.') {
        v = v.get(part)?;
    }
    Some(v).filter(|v| !v.is_null())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_categories_exist() {
        let mut seen = std::collections::HashSet::new();
        for i in ITEMS {
            assert!(seen.insert(i.key), "duplicate key {}", i.key);
            assert!(i.cat < CATEGORIES.len());
        }
    }

    #[test]
    fn lookup_follows_dots_and_skips_null() {
        let c = serde_json::json!({"tui": {"notifications": true, "pet": null}});
        assert_eq!(lookup(&c, "tui.notifications"), Some(&serde_json::json!(true)));
        assert!(lookup(&c, "tui.pet").is_none());
        assert!(lookup(&c, "history.max_bytes").is_none());
    }
}

//! UI strings for the config GUI, with English + Japanese translations.
//!
//! Selection order:
//! 1. `HKCU\Software\ArcThumb\Language` registry override (`"en"` | `"ja"`).
//! 2. OS default locale via `GetUserDefaultLocaleName` — starts with `"ja"` → Japanese.
//! 3. English fallback.
//!
//! Strings are handed to the Slint UI at startup via `in` properties.
//! A future refactor may move them into `.slint` `@tr("...")` with
//! gettext once the gettext toolchain (`xgettext`/`msgfmt`) is wired
//! into the build.

use winreg::RegKey;
use winreg::enums::*;

pub struct Strings {
    pub window_title: &'static str,
    pub menu_file: &'static str,
    pub menu_file_exit: &'static str,
    pub menu_help: &'static str,
    pub menu_help_about: &'static str,
    pub group_extensions: &'static str,
    pub group_sort: &'static str,
    pub sort_natural: &'static str,
    pub sort_alphabetical: &'static str,
    pub cb_prefer_cover: &'static str,
    pub cb_enable_preview: &'static str,
    pub btn_ok: &'static str,
    pub btn_cancel: &'static str,
    pub btn_apply: &'static str,
    pub btn_regenerate: &'static str,
    pub btn_close: &'static str,
    pub about_title: &'static str,
    pub about_body: &'static str,
    pub regen_confirm: &'static str,
    pub regen_done: &'static str,
    pub regen_partial: &'static str,
    pub error_title: &'static str,
    pub error_save: &'static str,
    pub error_register: &'static str,
    // Update check dialog
    pub update_title: &'static str,
    pub update_available: &'static str,
    pub update_skip_checkbox: &'static str,
    pub update_btn_open: &'static str,
    pub update_btn_later: &'static str,
    // Donation dialog
    pub donation_title: &'static str,
    pub donation_prompt: &'static str,
    pub donation_dont_show_checkbox: &'static str,
    pub donation_btn_sponsor: &'static str,
    pub donation_btn_later: &'static str,
}

pub const EN: Strings = Strings {
    window_title: "ArcThumb Configuration",
    menu_file: "File",
    menu_file_exit: "Exit",
    menu_help: "Help",
    menu_help_about: "About ArcThumb",
    group_extensions: "Enabled extensions",
    group_sort: "Sort order",
    sort_natural: "Natural (page2 < page10)",
    sort_alphabetical: "Alphabetical",
    cb_prefer_cover: "Prefer cover / folder / thumb / thumbnail / front",
    cb_enable_preview: "Enable preview pane (Alt+P)",
    btn_ok: "OK",
    btn_cancel: "Cancel",
    btn_apply: "Apply",
    btn_regenerate: "Regenerate thumbnails",
    btn_close: "Close",
    about_title: "About ArcThumb",
    about_body: "ArcThumb — archive thumbnail provider for Windows Explorer.\n\nThis application uses Slint (https://slint.dev) under the Slint Royalty-Free License 2.0.",
    regen_confirm: "This will close all Explorer windows, delete the Windows thumbnail and icon caches, and restart Explorer.\n\nUse this if archive thumbnails are still missing after installing or enabling new file types.\n\nContinue?",
    regen_done: "Thumbnail cache cleared and Explorer restarted.\n\nNew thumbnails will be generated as you browse.",
    regen_partial: "Some cache files were locked and could not be deleted. Try closing other applications and run this again.",
    error_title: "ArcThumb",
    error_save: "Failed to save settings to the registry.",
    error_register: "Failed to update shell extension registration.",
    update_title: "Update available",
    update_available: "A new version of ArcThumb is available: v{}  (current: v{})",
    update_skip_checkbox: "Skip this version",
    update_btn_open: "Open download page",
    update_btn_later: "Remind me later",
    donation_title: "Thank you for updating!",
    donation_prompt: "ArcThumb has been updated to v{}.\nWould you like to support development?",
    donation_dont_show_checkbox: "Don't show this again",
    donation_btn_sponsor: "Open sponsor page",
    donation_btn_later: "Maybe next time",
};

pub const JA: Strings = Strings {
    window_title: "ArcThumb 設定",
    menu_file: "ファイル",
    menu_file_exit: "終了",
    menu_help: "ヘルプ",
    menu_help_about: "ArcThumb について",
    group_extensions: "有効にする拡張子",
    group_sort: "並び順",
    sort_natural: "自然順 (page2 < page10)",
    sort_alphabetical: "アルファベット順",
    cb_prefer_cover: "cover / folder / thumb / thumbnail / front を優先",
    cb_enable_preview: "プレビュー ウィンドウを有効にする (Alt+P)",
    btn_ok: "OK",
    btn_cancel: "キャンセル",
    btn_apply: "適用",
    btn_regenerate: "サムネイルを再生成",
    btn_close: "閉じる",
    about_title: "ArcThumb について",
    about_body: "ArcThumb — Windows エクスプローラー向けのアーカイブサムネイル プロバイダー。\n\nこのアプリケーションは Slint (https://slint.dev) を Slint Royalty-Free License 2.0 に基づいて使用しています。",
    regen_confirm: "エクスプローラーのウィンドウをすべて閉じ、Windows のサムネイル/アイコンキャッシュを削除してエクスプローラーを再起動します。\n\nインストール後や対応拡張子を有効にしたあとでサムネイルが表示されない場合に使ってください。\n\n続行しますか？",
    regen_done: "サムネイルキャッシュを削除し、エクスプローラーを再起動しました。\n\nフォルダを開くと新しいサムネイルが作成されます。",
    regen_partial: "一部のキャッシュファイルがロックされていて削除できませんでした。他のアプリを閉じてから、もう一度実行してください。",
    error_title: "ArcThumb",
    error_save: "設定の保存に失敗しました。",
    error_register: "シェル拡張の登録状態の更新に失敗しました。",
    update_title: "アップデート通知",
    update_available: "ArcThumb の新しいバージョンがあります: v{}  (現在: v{})",
    update_skip_checkbox: "このバージョンをスキップ",
    update_btn_open: "ダウンロードページを開く",
    update_btn_later: "あとで通知",
    donation_title: "アップデートありがとうございます！",
    donation_prompt: "ArcThumb v{} にアップデートされました。\n開発を支援しますか？",
    donation_dont_show_checkbox: "今後表示しない",
    donation_btn_sponsor: "スポンサーページを開く",
    donation_btn_later: "また今度",
};

/// Resolve the UI language to use right now.
pub fn current() -> &'static Strings {
    // 1. Registry override
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\ArcThumb")
        && let Ok(lang) = key.get_value::<String, _>("Language")
    {
        match lang.to_ascii_lowercase().as_str() {
            "en" | "english" => return &EN,
            "ja" | "japanese" | "jp" => return &JA,
            _ => {}
        }
    }

    // 2. OS default locale
    if detect_os_locale_is_japanese() {
        return &JA;
    }

    // 3. Fallback
    &EN
}

fn detect_os_locale_is_japanese() -> bool {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;

    // LOCALE_NAME_MAX_LENGTH = 85
    let mut buf = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if len <= 0 {
        return false;
    }
    let end = (len as usize).saturating_sub(1);
    let s = String::from_utf16_lossy(&buf[..end]);
    s.to_ascii_lowercase().starts_with("ja")
}

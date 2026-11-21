use super::UiState;
use libadwaita as adw;

impl UiState {
    pub(super) fn show_remote_desktop(&self) {
        if std::env::var_os("FORGE_SAFE_MODE").is_some() {
            self.toast_overlay.add_toast(adw::Toast::new(
                "Remote connections are disabled in safe mode.",
            ));
            return;
        }
        crate::jwm_remote_gtk::present(&self.window, &self.remote_desktop_window);
    }
}

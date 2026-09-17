//! Composing the Settings screen.
//!
//! [`SettingsScreen`] holds a snapshot of what [`SettingsHost`] reported plus
//! the small amount of UI-only state a press can change: which page is
//! showing, and which destructive action (if any) is waiting on a
//! confirmation. Nothing here calls into the Host except [`Self::confirm`] —
//! opening a dialog is free, and going through with it is the one place a
//! transaction actually happens (§6).

use paper_packages::{AppId, Capability};
use paper_sdk::chrome;
use paper_sdk::{Canvas, Point, Rect};

use crate::confirm::{self, ConfirmDialog, ConfirmLayout};
use crate::host::{
    CatalogStatus, DiagnosticEntry, GrantSummary, HostOpError, InstalledAppSummary, PlatformInfo,
    SettingsHost, StorageUsage,
};
use crate::nav::{self, NavLayout, SettingsPage};
use crate::pages::{self, AppsLayout, GrantsLayout};

/// What confirming the open dialog would do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingKind {
    Rollback(AppId),
    Uninstall(AppId),
    Revoke(AppId, Capability),
}

/// A destructive action waiting on its confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingAction {
    dialog: ConfirmDialog,
    kind: PendingKind,
}

/// What the Settings screen is showing.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsScreen {
    /// Which page is current.
    pub page: SettingsPage,
    apps: Vec<InstalledAppSummary>,
    storage: StorageUsage,
    grants: Vec<GrantSummary>,
    catalog: CatalogStatus,
    platform: PlatformInfo,
    diagnostics: Vec<DiagnosticEntry>,
    pending: Option<PendingAction>,
}

impl SettingsScreen {
    /// Snapshots everything `host` reports right now, showing the apps page.
    pub fn from_host(host: &dyn SettingsHost) -> Self {
        Self {
            page: SettingsPage::Apps,
            apps: host.installed_apps(),
            storage: host.storage_usage(),
            grants: host.grants(),
            catalog: host.catalog_status(),
            platform: host.platform_info(),
            diagnostics: host.diagnostics(),
            pending: None,
        }
    }

    /// Whether a confirmation is currently blocking the page underneath it.
    pub fn is_confirming(&self) -> bool {
        self.pending.is_some()
    }

    /// Handles a press on a nav tab. Ignored while a confirmation is open —
    /// the modal has to be resolved before anything else responds.
    pub fn press_tab(&mut self, nav: &NavLayout, at: Point) {
        if self.pending.is_some() {
            return;
        }
        if let Some(page) = nav.hit_test(at) {
            self.page = page;
        }
    }

    /// Handles a press on the apps page.
    pub fn press_apps(&mut self, layout: &AppsLayout, at: Point) {
        if self.pending.is_some() {
            return;
        }
        if let Some(index) = layout.uninstall_hit(at) {
            self.begin_uninstall(index);
        } else if let Some(index) = layout.rollback_hit(at) {
            self.begin_rollback(index);
        }
    }

    /// Handles a press on the grants page.
    pub fn press_grants(&mut self, layout: &GrantsLayout, at: Point) {
        if self.pending.is_some() {
            return;
        }
        if let Some(index) = layout.revoke_hit(at) {
            self.begin_revoke(index);
        }
    }

    /// Opens the rollback confirmation for `apps[index]`.
    ///
    /// Does nothing when the app has no previous release — there is nothing
    /// to confirm, and the apps page never draws a rollback button for that
    /// row in the first place (`AppsLayout::rollback_hit` cannot return this
    /// index), so this guard is what keeps a stale or synthetic layout from
    /// opening a dialog rollback cannot honour.
    fn begin_rollback(&mut self, index: usize) {
        let Some(app) = self.apps.get(index) else {
            return;
        };
        let Some(previous) = app.previous_version.clone() else {
            return;
        };
        self.pending = Some(PendingAction {
            dialog: ConfirmDialog::new(
                format!("ROLL BACK {}", app.name.to_uppercase()),
                vec![format!("RETURNS TO V{previous}.")],
                "ROLL BACK",
            ),
            kind: PendingKind::Rollback(app.id.clone()),
        });
    }

    /// Opens the uninstall confirmation for `apps[index]`, stating plainly
    /// what data would be lost.
    fn begin_uninstall(&mut self, index: usize) {
        let Some(app) = self.apps.get(index) else {
            return;
        };
        let lines = if app.data_bytes > 0 {
            vec![
                format!(
                    "{} OF DATA WILL BE DELETED.",
                    pages::format_bytes(app.data_bytes)
                ),
                "THIS CANNOT BE UNDONE.".to_owned(),
            ]
        } else {
            vec!["NO DATA WILL BE LOST.".to_owned()]
        };
        self.pending = Some(PendingAction {
            dialog: ConfirmDialog::new(
                format!("UNINSTALL {}", app.name.to_uppercase()),
                lines,
                "UNINSTALL",
            ),
            kind: PendingKind::Uninstall(app.id.clone()),
        });
    }

    /// Opens the revoke confirmation for `grants[index]`, warning when the
    /// app is relying on the grant right now rather than only eventually.
    fn begin_revoke(&mut self, index: usize) {
        let Some(grant) = self.grants.get(index) else {
            return;
        };
        let lines = if grant.in_use {
            vec![
                format!("{} IS USING THIS RIGHT NOW.", grant.app_name.to_uppercase()),
                "REVOKING MAY INTERRUPT IT.".to_owned(),
            ]
        } else {
            vec![format!(
                "{} WILL LOSE {} ACCESS.",
                grant.app_name.to_uppercase(),
                grant.capability.name().to_uppercase()
            )]
        };
        self.pending = Some(PendingAction {
            dialog: ConfirmDialog::new("REVOKE GRANT", lines, "REVOKE"),
            kind: PendingKind::Revoke(grant.app_id.clone(), grant.capability),
        });
    }

    /// Backs out of the open confirmation without calling the Host.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Goes through with the open confirmation and refreshes the snapshot
    /// from `host`. Does nothing if nothing is pending.
    pub fn confirm(&mut self, host: &mut dyn SettingsHost) -> Result<(), HostOpError> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let result = match pending.kind {
            PendingKind::Rollback(id) => host.rollback(&id),
            PendingKind::Uninstall(id) => host.uninstall(&id),
            PendingKind::Revoke(id, capability) => host.revoke_grant(&id, capability),
        };
        self.refresh(&*host);
        result
    }

    fn refresh(&mut self, host: &dyn SettingsHost) {
        self.apps = host.installed_apps();
        self.storage = host.storage_usage();
        self.grants = host.grants();
        self.catalog = host.catalog_status();
        self.platform = host.platform_info();
        self.diagnostics = host.diagnostics();
    }
}

/// Which page's interactive layout was drawn.
#[derive(Debug, Clone, PartialEq)]
pub enum PageLayout {
    /// The apps page, with a rollback/uninstall button per row.
    Apps(AppsLayout),
    /// The grants page, with a revoke button per row.
    Grants(GrantsLayout),
    /// A read-only page: storage, catalog, platform or diagnostics.
    ReadOnly,
}

/// Where everything on the Settings screen landed.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsLayout {
    /// The tab strip.
    pub nav: NavLayout,
    /// The current page's interactive layout, if it has one.
    pub page: PageLayout,
    /// The always-available return action, absent while a confirmation is
    /// open — the scrim covers it, and a press should not reach through.
    pub return_to_stock: Option<Rect>,
    /// The open confirmation's two actions, if one is open.
    pub confirm: Option<ConfirmLayout>,
}

/// Draws the Settings screen and returns the layout used, so the caller can
/// hit-test presses against exactly what was drawn.
pub fn render(canvas: &mut Canvas, screen: &SettingsScreen) -> SettingsLayout {
    canvas.clear(paper_sdk::palette::PAPER);
    let content = chrome::draw_status_bar(
        canvas,
        "SETTINGS",
        &format!("V{}", screen.platform.paperclip_version),
    );
    let (nav_layout, area) = nav::draw_nav(canvas, screen.page, content.y);

    let page = match screen.page {
        SettingsPage::Apps => PageLayout::Apps(pages::draw_apps(canvas, area, &screen.apps)),
        SettingsPage::Storage => {
            pages::draw_storage(canvas, area, &screen.storage);
            PageLayout::ReadOnly
        }
        SettingsPage::Grants => {
            PageLayout::Grants(pages::draw_grants(canvas, area, &screen.grants))
        }
        SettingsPage::Catalog => {
            pages::draw_catalog(canvas, area, &screen.catalog);
            PageLayout::ReadOnly
        }
        SettingsPage::Platform => {
            pages::draw_platform(canvas, area, &screen.platform);
            PageLayout::ReadOnly
        }
        SettingsPage::Diagnostics => {
            pages::draw_diagnostics(canvas, area, &screen.diagnostics);
            PageLayout::ReadOnly
        }
    };

    let return_to_stock = if screen.pending.is_none() {
        chrome::draw_footer_actions(canvas, &["RETURN TO REMARKABLE"])
            .into_iter()
            .next()
    } else {
        None
    };

    let confirm = screen
        .pending
        .as_ref()
        .map(|pending| confirm::draw_confirm(canvas, &pending.dialog));

    SettingsLayout {
        nav: nav_layout,
        page,
        return_to_stock,
        confirm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::PlaceholderHost;
    use paper_sdk::SCREEN;

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    fn chess_id() -> AppId {
        "dev.calum.chess".parse().expect("valid fixture id")
    }

    #[test]
    fn attempting_rollback_with_no_previous_release_opens_nothing() {
        let mut host = PlaceholderHost::new();
        host.rollback(&chess_id())
            .expect("first rollback clears the previous release");
        let mut screen = SettingsScreen::from_host(&host);
        assert!(!screen.apps[0].has_previous_release());

        screen.begin_rollback(0);
        assert!(
            screen.pending.is_none(),
            "nothing to roll back to should not open a dialog"
        );
    }

    #[test]
    fn rollback_with_a_previous_release_opens_a_dialog_naming_it() {
        let host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        screen.begin_rollback(0);
        let pending = screen
            .pending
            .as_ref()
            .expect("a previous release should open a dialog");
        assert!(
            pending
                .dialog
                .lines
                .iter()
                .any(|line| line.contains("0.1.0"))
        );
    }

    #[test]
    fn uninstalling_an_app_with_data_present_warns_about_losing_it() {
        let host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        assert!(
            screen.apps[0].data_bytes > 0,
            "the fixture should have data to lose"
        );

        screen.begin_uninstall(0);
        let pending = screen
            .pending
            .as_ref()
            .expect("uninstall always opens a dialog");
        assert!(pending.dialog.lines.iter().any(|line| line.contains("MB")));
        assert!(
            pending
                .dialog
                .lines
                .iter()
                .any(|line| line.contains("CANNOT BE UNDONE")),
            "a destructive action must say it cannot be undone"
        );
    }

    #[test]
    fn uninstalling_an_app_with_no_data_does_not_claim_data_will_be_lost() {
        let host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        screen.apps[0].data_bytes = 0;

        screen.begin_uninstall(0);
        let pending = screen
            .pending
            .as_ref()
            .expect("uninstall always opens a dialog");
        assert!(pending.dialog.lines.iter().all(|line| !line.contains("MB")));
    }

    #[test]
    fn revoking_a_grant_currently_in_use_warns_about_interrupting_it() {
        let host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        assert!(
            screen.grants[0].in_use,
            "the fixture's first grant should be in use"
        );

        screen.begin_revoke(0);
        let pending = screen
            .pending
            .as_ref()
            .expect("revoke always opens a dialog");
        assert!(
            pending
                .dialog
                .lines
                .iter()
                .any(|line| line.contains("RIGHT NOW"))
        );
    }

    #[test]
    fn revoking_a_grant_not_in_use_does_not_warn_about_interrupting_it() {
        let host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        assert!(
            !screen.grants[1].in_use,
            "the fixture's second grant should be idle"
        );

        screen.begin_revoke(1);
        let pending = screen
            .pending
            .as_ref()
            .expect("revoke always opens a dialog");
        assert!(
            pending
                .dialog
                .lines
                .iter()
                .all(|line| !line.contains("RIGHT NOW"))
        );
    }

    #[test]
    fn cancelling_a_pending_action_does_not_call_the_host() {
        let mut host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        screen.begin_uninstall(0);
        screen.cancel();
        assert!(screen.pending.is_none());
        assert_eq!(
            screen.confirm(&mut host),
            Ok(()),
            "nothing pending, nothing to do"
        );
        assert_eq!(
            host.installed_apps().len(),
            1,
            "cancelling must not touch the host"
        );
    }

    #[test]
    fn confirming_an_uninstall_calls_the_host_and_refreshes_the_snapshot() {
        let mut host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        screen.begin_uninstall(0);
        assert_eq!(screen.confirm(&mut host), Ok(()));
        assert!(screen.pending.is_none());
        assert!(
            screen.apps.is_empty(),
            "the snapshot should reflect the uninstall"
        );
        assert!(host.installed_apps().is_empty());
    }

    #[test]
    fn a_press_on_a_tab_switches_the_page_unless_a_dialog_is_open() {
        let host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        let mut canvas = canvas();
        let layout = render(&mut canvas, &screen);

        let storage_tab = layout.nav.tabs()[1];
        screen.press_tab(&layout.nav, storage_tab.center());
        assert_eq!(screen.page, SettingsPage::Storage);

        screen.begin_uninstall(0);
        let apps_tab = layout.nav.tabs()[0];
        screen.press_tab(&layout.nav, apps_tab.center());
        assert_eq!(
            screen.page,
            SettingsPage::Storage,
            "a tab press must not reach through a dialog"
        );
    }

    #[test]
    fn the_footer_return_action_disappears_while_confirming() {
        let host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        let mut canvas = canvas();
        assert!(render(&mut canvas, &screen).return_to_stock.is_some());

        screen.begin_uninstall(0);
        let layout = render(&mut canvas, &screen);
        assert!(layout.return_to_stock.is_none());
        assert!(layout.confirm.is_some());
    }

    #[test]
    fn every_page_renders_without_going_blank_or_solid() {
        let host = PlaceholderHost::new();
        let mut screen = SettingsScreen::from_host(&host);
        for page in SettingsPage::ALL {
            screen.page = page;
            let mut canvas = canvas();
            render(&mut canvas, &screen);
            let coverage = canvas.ink_coverage();
            assert!(coverage > 0.01, "{page:?} drew almost nothing ({coverage})");
            assert!(coverage < 0.9, "{page:?} went nearly solid ({coverage})");
        }
    }
}

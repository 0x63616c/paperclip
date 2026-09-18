//! Composing the Settings screen.
//!
//! [`SettingsScreen`] holds a cached snapshot of what the host last answered
//! plus the small amount of UI-only state a press can change: which page is
//! showing, and which destructive action (if any) is waiting on a
//! confirmation. It calls nobody itself (§6, WWW-71, ADR-0028): every read
//! and write is a [`paper_sdk::AdminQuery`] [`crate::app::SettingsApp`] sends
//! and applies, because the host answers asynchronously and a screen that
//! blocked on one would be blocking the event loop it is drawn from.

use paper_sdk::{
    Action, AdminQuery, AdminValue, AppId, Canvas, Capability, CatalogStatus, DiagnosticEntry,
    GrantSummary, InstalledAppSummary, PlatformFact, Point, PointerEvent, Rect, Size,
    StorageBucket, StorageUsage, chrome,
};

use crate::confirm::{self, ConfirmDialog, ConfirmLayout};
use crate::nav::{self, NavLayout, SettingsPage};
use crate::pages::{self, AppsLayout, GrantsLayout};

/// What confirming the open dialog would ask the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingKind {
    Rollback(AppId),
    Uninstall(AppId),
    Revoke(AppId, Capability),
}

impl PendingKind {
    /// The query that carries this action to the host.
    pub(crate) fn into_query(self) -> AdminQuery {
        match self {
            PendingKind::Rollback(app) => AdminQuery::Rollback { app },
            PendingKind::Uninstall(app) => AdminQuery::Uninstall { app },
            PendingKind::Revoke(app, capability) => AdminQuery::RevokeGrant { app, capability },
        }
    }
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
    platform: PlatformFact,
    diagnostics: Vec<DiagnosticEntry>,
    pending: Option<PendingAction>,
}

/// What one press decided, for [`crate::app::SettingsApp`] to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press {
    /// What the platform should do next.
    pub action: Action,
    /// A write to ask the host for, when this press confirmed a destructive
    /// action. The dialog has already closed by the time this is `Some` —
    /// see the module doc on why a press cannot wait for the host's answer.
    pub(crate) admin: Option<PendingKind>,
}

impl Press {
    /// A press that changed nothing.
    fn ignored() -> Self {
        Self {
            action: Action::None,
            admin: None,
        }
    }

    /// A press that changed what is on screen and nothing else.
    fn redraw() -> Self {
        Self {
            action: Action::Redraw,
            admin: None,
        }
    }
}

/// The placeholder text shown for a fact this screen has not been told yet.
const LOADING: &str = "\u{2026}";

impl SettingsScreen {
    /// A screen with nothing loaded yet, showing the apps page.
    ///
    /// [`crate::app::SettingsApp`] fills this in as its admin queries are
    /// answered ([`Self::apply`]) — there is no synchronous constructor that
    /// reads a real store, because there is no longer a store this crate can
    /// reach directly (WWW-71).
    pub fn loading() -> Self {
        Self {
            page: SettingsPage::Apps,
            apps: Vec::new(),
            storage: StorageUsage {
                free_bytes: 0,
                buckets: Vec::new(),
            },
            grants: Vec::new(),
            catalog: CatalogStatus {
                endpoint: LOADING.to_owned(),
                last_fetch: None,
                reachable: false,
            },
            platform: PlatformFact {
                paperclip_version: env!("CARGO_PKG_VERSION").to_owned(),
                firmware: LOADING.to_owned(),
                active_release: LOADING.to_owned(),
            },
            diagnostics: Vec::new(),
            pending: None,
        }
    }

    /// A screen pre-populated with fixture data.
    ///
    /// Not device evidence: every value here is invented so the pages have
    /// something legible to draw without a real host answering. Used by the
    /// desktop preview (`paperctl preview`/`paperctl open`, which render a
    /// screen directly rather than through a session) and by this crate's own
    /// tests, so there is exactly one fixture rather than one per caller.
    pub fn preview() -> Self {
        let chess: AppId = "dev.calum.chess".parse().expect("valid fixture id");
        Self {
            page: SettingsPage::Apps,
            apps: vec![InstalledAppSummary {
                id: chess.clone(),
                name: "Chess".to_owned(),
                active_version: "0.2.0".to_owned(),
                previous_version: Some("0.1.0".to_owned()),
                data_bytes: 2_400_000,
            }],
            storage: StorageUsage {
                free_bytes: 41_300_000,
                buckets: vec![
                    StorageBucket {
                        label: "DATA / CHESS".to_owned(),
                        bytes: 2_400_000,
                    },
                    StorageBucket {
                        label: "SHARED".to_owned(),
                        bytes: 0,
                    },
                    StorageBucket {
                        label: "STAGING".to_owned(),
                        bytes: 0,
                    },
                    StorageBucket {
                        label: "RELEASES".to_owned(),
                        bytes: 18_700_000,
                    },
                ],
            },
            grants: vec![
                GrantSummary {
                    app_id: chess.clone(),
                    app_name: "Chess".to_owned(),
                    capability: Capability::Storage,
                    in_use: true,
                },
                GrantSummary {
                    app_id: chess,
                    app_name: "Chess".to_owned(),
                    capability: Capability::Sharing,
                    in_use: false,
                },
            ],
            catalog: CatalogStatus {
                endpoint: "HTTPS://CATALOG.LOCAL".to_owned(),
                last_fetch: Some("2026-09-16 21:04".to_owned()),
                reachable: true,
            },
            platform: PlatformFact {
                paperclip_version: "0.1.0".to_owned(),
                firmware: "3.28.0.172".to_owned(),
                active_release: "STAGE 4".to_owned(),
            },
            diagnostics: vec![DiagnosticEntry {
                when: "2026-09-16 21:04".to_owned(),
                message: "NO ERRORS RECORDED THIS SESSION".to_owned(),
            }],
            pending: None,
        }
    }

    /// Files one admin answer's value under the field it belongs to.
    ///
    /// A write's own `Result` is not applied here: whether a rollback,
    /// uninstall or revoke succeeded is [`crate::app::SettingsApp`]'s
    /// business (it decides whether to warn and which reads to refresh),
    /// not this screen's — this screen only ever shows what the host most
    /// recently reported, never a write's outcome directly.
    pub fn apply(&mut self, value: AdminValue) {
        match value {
            AdminValue::InstalledApps(apps) => self.apps = apps,
            AdminValue::StorageUsage(storage) => self.storage = storage,
            AdminValue::Grants(grants) => self.grants = grants,
            AdminValue::CatalogStatus(catalog) => self.catalog = catalog,
            AdminValue::PlatformInfo(platform) => self.platform = platform,
            AdminValue::Diagnostics(diagnostics) => self.diagnostics = diagnostics,
            AdminValue::Rollback(_) | AdminValue::Uninstall(_) | AdminValue::RevokeGrant(_) => {}
            // `AdminValue` is `#[non_exhaustive]`: a future minor protocol
            // bump can add a variant this build predates.
            _ => {}
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

    /// Backs out of the open confirmation without asking the host anything.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Closes the open confirmation and hands back what it asked for, if
    /// anything was pending.
    ///
    /// The dialog closes here, before the host has answered: see the module
    /// doc. [`crate::app::SettingsApp`] is the caller, and turns `Some` into
    /// the [`AdminQuery`] that actually performs the write.
    pub(crate) fn confirm(&mut self) -> Option<PendingKind> {
        self.pending.take().map(|pending| pending.kind)
    }

    /// Handles one pointer event against the layout that was drawn for it.
    ///
    /// The single entry point every caller uses — the app and the desktop
    /// preview both route presses through this, so "what a tap does" has one
    /// implementation rather than one per host (ADR-0018).
    ///
    /// Order matters and is the modal rule: while a confirmation is open it
    /// absorbs every press, including one that lands on the page underneath
    /// its scrim.
    pub fn press(&mut self, layout: &SettingsLayout, pointer: &PointerEvent) -> Press {
        if !pointer.is_tap() {
            return Press::ignored();
        }
        let at = pointer.at;

        if let Some(confirm) = &layout.confirm {
            if confirm.cancel.contains(at) {
                self.cancel();
                return Press::redraw();
            }
            if confirm.confirm.contains(at) {
                return Press {
                    action: Action::Redraw,
                    admin: self.confirm(),
                };
            }
            // The scrim. A press here resolves nothing, and must not reach
            // the page it is covering.
            return Press::ignored();
        }

        if let Some(rect) = layout.return_to_stock
            && rect.contains(at)
        {
            return Press {
                action: Action::ReturnToStock,
                admin: None,
            };
        }

        let page = self.page;
        self.press_tab(&layout.nav, at);
        if self.page != page {
            return Press::redraw();
        }

        match &layout.page {
            PageLayout::Apps(apps) => self.press_apps(apps, at),
            PageLayout::Grants(grants) => self.press_grants(grants, at),
            PageLayout::ReadOnly => {}
        }
        if self.is_confirming() {
            Press::redraw()
        } else {
            Press::ignored()
        }
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

/// Where everything on the Settings screen would land for a viewport shaped
/// like `bounds`, computed from `screen` alone — no [`Canvas`].
///
/// [`crate::app::SettingsApp`] calls this directly from
/// [`event`](paper_sdk::App::event), so a tap is hit-testable before the
/// first [`draw`](paper_sdk::App::draw) ever runs rather than only after a
/// frame has been drawn to cache it from.
pub(crate) fn layout(bounds: Size, screen: &SettingsScreen) -> SettingsLayout {
    let bounds = Rect::new(0.0, 0.0, bounds.width as f32, bounds.height as f32);
    let content_y = chrome::status_bar_content_area(bounds).y;
    let (nav_layout, area) = nav::layout_nav(bounds, content_y);

    let page = match screen.page {
        SettingsPage::Apps => PageLayout::Apps(pages::layout_apps(area, &screen.apps)),
        SettingsPage::Storage => PageLayout::ReadOnly,
        SettingsPage::Grants => PageLayout::Grants(pages::layout_grants(area, &screen.grants)),
        SettingsPage::Catalog => PageLayout::ReadOnly,
        SettingsPage::Platform => PageLayout::ReadOnly,
        SettingsPage::Diagnostics => PageLayout::ReadOnly,
    };

    let return_to_stock = if screen.pending.is_none() {
        chrome::footer_action_rects(bounds, 1).into_iter().next()
    } else {
        None
    };

    let confirm = screen
        .pending
        .as_ref()
        .map(|pending| confirm::layout_confirm(bounds, pending.dialog.lines.len()));

    SettingsLayout {
        nav: nav_layout,
        page,
        return_to_stock,
        confirm,
    }
}

/// Draws the Settings screen against a layout [`layout`] already computed.
pub(crate) fn draw(canvas: &mut Canvas, screen: &SettingsScreen, layout: &SettingsLayout) {
    canvas.clear(paper_sdk::palette::PAPER);
    let content = chrome::draw_status_bar(
        canvas,
        "SETTINGS",
        &format!("V{}", screen.platform.paperclip_version),
    );
    let (_, area) = nav::layout_nav(canvas.bounds(), content.y);
    nav::draw(canvas, screen.page, &layout.nav);

    match (screen.page, &layout.page) {
        (SettingsPage::Apps, PageLayout::Apps(apps_layout)) => {
            pages::draw_apps(canvas, area, &screen.apps, apps_layout);
        }
        (SettingsPage::Storage, _) => pages::draw_storage(canvas, area, &screen.storage),
        (SettingsPage::Grants, PageLayout::Grants(grants_layout)) => {
            pages::draw_grants(canvas, area, &screen.grants, grants_layout);
        }
        (SettingsPage::Catalog, _) => pages::draw_catalog(canvas, area, &screen.catalog),
        (SettingsPage::Platform, _) => pages::draw_platform(canvas, area, &screen.platform),
        (SettingsPage::Diagnostics, _) => {
            pages::draw_diagnostics(canvas, area, &screen.diagnostics)
        }
        // `layout` was computed from the same `screen.page` `draw` is being
        // asked to draw, so the interactive variant always matches the page.
        (SettingsPage::Apps | SettingsPage::Grants, _) => {
            unreachable!("layout() and draw() were called with different SettingsScreen values")
        }
    }

    if screen.pending.is_none() {
        chrome::draw_footer_actions(canvas, &["RETURN TO REMARKABLE"]);
    }

    if let Some(pending) = &screen.pending {
        let confirm_layout = layout
            .confirm
            .as_ref()
            .expect("layout() computed a confirmation layout whenever screen.pending is Some");
        confirm::draw_confirm(canvas, &pending.dialog, confirm_layout);
    }
}

/// Computes the layout and draws it, for callers that want both — every
/// existing call site, and every test that predates the split above.
pub fn render(canvas: &mut Canvas, screen: &SettingsScreen) -> SettingsLayout {
    let bounds = canvas.bounds();
    let layout = self::layout(Size::new(bounds.width as u32, bounds.height as u32), screen);
    draw(canvas, screen, &layout);
    layout
}

#[cfg(test)]
mod tests {
    use super::*;
    use paper_sdk::AdminError;
    use paper_sdk::SCREEN;

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    fn chess_id() -> AppId {
        "dev.calum.chess".parse().expect("valid fixture id")
    }

    #[test]
    fn layout_needs_no_canvas_and_matches_what_render_draws_for_every_page() {
        for page in SettingsPage::ALL {
            let screen = SettingsScreen {
                page,
                ..SettingsScreen::preview()
            };
            let drawn = render(&mut canvas(), &screen);
            assert_eq!(layout(SCREEN, &screen), drawn, "{page:?}");
        }
    }

    #[test]
    fn attempting_rollback_with_no_previous_release_opens_nothing() {
        let mut screen = SettingsScreen::preview();
        screen.apps[0].previous_version = None;

        screen.begin_rollback(0);
        assert!(
            screen.pending.is_none(),
            "nothing to roll back to should not open a dialog"
        );
    }

    #[test]
    fn rollback_with_a_previous_release_opens_a_dialog_naming_it() {
        let mut screen = SettingsScreen::preview();
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
        let mut screen = SettingsScreen::preview();
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
        let mut screen = SettingsScreen::preview();
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
        let mut screen = SettingsScreen::preview();
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
        let mut screen = SettingsScreen::preview();
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
    fn cancelling_a_pending_action_asks_the_host_nothing() {
        let mut screen = SettingsScreen::preview();
        screen.begin_uninstall(0);
        screen.cancel();
        assert!(screen.pending.is_none());
        assert_eq!(screen.confirm(), None, "nothing pending, nothing to ask");
    }

    #[test]
    fn confirming_a_destructive_action_closes_the_dialog_and_names_the_write() {
        let mut screen = SettingsScreen::preview();
        screen.begin_uninstall(0);
        let kind = screen.confirm();
        assert!(screen.pending.is_none());
        assert_eq!(kind, Some(PendingKind::Uninstall(chess_id())));
    }

    #[test]
    fn applying_an_admin_value_updates_the_matching_field_and_nothing_else() {
        let mut screen = SettingsScreen::loading();
        assert!(screen.apps.is_empty());

        screen.apply(AdminValue::InstalledApps(vec![InstalledAppSummary {
            id: chess_id(),
            name: "Chess".to_owned(),
            active_version: "0.2.0".to_owned(),
            previous_version: None,
            data_bytes: 0,
        }]));
        assert_eq!(screen.apps.len(), 1);
        assert!(
            screen.grants.is_empty(),
            "an InstalledApps answer must not touch grants"
        );
    }

    #[test]
    fn a_write_s_own_result_is_not_applied_to_the_screen() {
        let mut screen = SettingsScreen::preview();
        let apps_before = screen.apps.clone();
        screen.apply(AdminValue::Uninstall(Err(AdminError::NotInstalled)));
        assert_eq!(
            screen.apps, apps_before,
            "a write's Result carries no display data of its own"
        );
    }

    #[test]
    fn a_press_on_a_tab_switches_the_page_unless_a_dialog_is_open() {
        let mut screen = SettingsScreen::preview();
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
        let mut screen = SettingsScreen::preview();
        let mut canvas = canvas();
        assert!(render(&mut canvas, &screen).return_to_stock.is_some());

        screen.begin_uninstall(0);
        let layout = render(&mut canvas, &screen);
        assert!(layout.return_to_stock.is_none());
        assert!(layout.confirm.is_some());
    }

    #[test]
    fn every_page_renders_without_going_blank_or_solid() {
        let mut screen = SettingsScreen::preview();
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

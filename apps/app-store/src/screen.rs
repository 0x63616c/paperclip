//! What the App Store is showing, and what a press changes about it.
//!
//! No I/O, on purpose. Every method here is a pure state transition over a
//! snapshot, so the whole of the App Store's behaviour — which button appears,
//! what a press does, what happens when a download fails — is testable without
//! a catalog, a store or a device.
//!
//! A press never performs work. It returns a [`Request`], which the app hands
//! to [`Context::spawn`](paper_sdk::Context::spawn); the answer comes back as
//! an [`Outcome`] and [`AppStoreScreen::apply`] folds it in. That is the shape
//! §8 requires — `event` must not block — and it is also what keeps a slow
//! catalog from freezing a list a person is scrolling.

use paper_packages::inventory::{AppEntry, AppState, Inventory};
use paper_packages::{AppId, DisplayName};
use semver::Version;

/// Work the app should do off the UI loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Re-read the store and the catalog.
    Refresh,
    /// Fetch one version's release notes.
    Notes {
        /// Which app.
        app: AppId,
        /// Which version.
        version: Version,
    },
    /// Install or update, explicitly asked for.
    Install {
        /// Which app.
        app: AppId,
        /// Which version.
        version: Version,
    },
    /// Select the previous release, explicitly asked for.
    Rollback {
        /// Which app.
        app: AppId,
    },
}

/// What a [`Request`] produced.
///
/// `Clone` so [`AppStoreApp`](crate::AppStoreApp) can fold one in from a
/// borrowed [`Event::Completed`](paper_sdk::Event::Completed) without owning
/// the event.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// A fresh snapshot.
    Refreshed(Box<Inventory>),
    /// Notes for the app and version that were asked about.
    Notes {
        /// Which app.
        app: AppId,
        /// The text.
        text: String,
    },
    /// An install finished and this version is now selected.
    Installed {
        /// Which app.
        app: AppId,
        /// What is selected now.
        version: Version,
    },
    /// A rollback finished.
    RolledBack {
        /// Which app.
        app: AppId,
        /// What is selected now.
        version: Version,
    },
    /// Something failed. Both lines come from `SourceError`.
    Failed {
        /// Which app, when the failure was about one.
        app: Option<AppId>,
        /// What happened.
        message: String,
        /// What the person can do about it.
        advice: &'static str,
    },
}

/// How far the operation in flight has got, as the screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Working {
    /// Which app it is about.
    pub app: AppId,
    /// The step, in words.
    pub step: String,
    /// Download fraction in percent, when there is one to show.
    ///
    /// `None` for every step that is not a download, rather than a number
    /// invented to keep a bar moving.
    pub percent: Option<u8>,
}

/// A failure the screen is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// What happened.
    pub message: String,
    /// What to do about it.
    pub advice: String,
}

/// Which view is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// The list of apps.
    List,
    /// One app, with its versions and notes.
    Detail(AppId),
}

/// The App Store's whole state.
#[derive(Debug, Clone, PartialEq)]
pub struct AppStoreScreen {
    view: View,
    inventory: Inventory,
    notes: Option<(AppId, String)>,
    working: Option<Working>,
    failure: Option<Failure>,
}

impl AppStoreScreen {
    /// A screen over a snapshot.
    pub fn new(inventory: Inventory) -> Self {
        Self {
            view: View::List,
            inventory,
            notes: None,
            working: None,
            failure: None,
        }
    }

    /// Which view is showing.
    pub fn view(&self) -> &View {
        &self.view
    }

    /// The snapshot behind the rows.
    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    /// The rows to draw.
    pub fn rows(&self) -> &[AppEntry] {
        self.inventory.entries()
    }

    /// The app the detail view is about, if that is what is showing.
    pub fn detail(&self) -> Option<&AppEntry> {
        match &self.view {
            View::Detail(app) => self.inventory.entry(app),
            View::List => None,
        }
    }

    /// The notes on screen, if they are for the app being shown.
    pub fn notes(&self) -> Option<&str> {
        let (app, text) = self.notes.as_ref()?;
        match &self.view {
            View::Detail(showing) if showing == app => Some(text.as_str()),
            _ => None,
        }
    }

    /// The operation in flight, if any.
    pub fn working(&self) -> Option<&Working> {
        self.working.as_ref()
    }

    /// The failure on screen, if any.
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }

    /// Whether the catalog could not be reached, so the rows may be behind.
    pub fn is_stale(&self) -> bool {
        self.inventory.is_stale()
    }

    /// A one-line summary of the catalog, for the status bar.
    pub fn catalog_summary(&self) -> String {
        match self.inventory.catalog() {
            Some(status) if status.stale => format!("{} \u{00B7} OFFLINE", status.name),
            Some(status) => status.name.clone(),
            None => "NO CATALOG".to_owned(),
        }
    }

    /// Opens one app's detail view, and asks for its notes if there are any to
    /// fetch.
    ///
    /// Returns `None` rather than a request when there is no offered version:
    /// notes live in a release descriptor, and an app the catalog does not
    /// offer has none to fetch.
    pub fn open(&mut self, app: &AppId) -> Option<Request> {
        let entry = self.inventory.entry(app)?;
        let version = entry.available.clone();
        self.view = View::Detail(app.clone());
        self.failure = None;
        self.notes = None;
        version.map(|version| Request::Notes {
            app: app.clone(),
            version,
        })
    }

    /// Goes back to the list.
    pub fn back(&mut self) {
        self.view = View::List;
        self.notes = None;
    }

    /// Dismisses the failure on screen.
    pub fn dismiss_failure(&mut self) {
        self.failure = None;
    }

    /// The action the primary button on `app`'s row would take.
    ///
    /// `None` when there is nothing to do: already current, or offered by
    /// nothing. A row with no action draws no button rather than a disabled
    /// one nobody can explain.
    pub fn primary_action(&self, app: &AppId) -> Option<Request> {
        let entry = self.inventory.entry(app)?;
        match entry.state {
            AppState::Available | AppState::UpdateAvailable => {
                entry.available.clone().map(|version| Request::Install {
                    app: app.clone(),
                    version,
                })
            }
            AppState::Failing if entry.can_roll_back() => {
                Some(Request::Rollback { app: app.clone() })
            }
            AppState::NothingSelected => {
                entry
                    .on_disk
                    .last()
                    .cloned()
                    .map(|version| Request::Install {
                        app: app.clone(),
                        version,
                    })
            }
            AppState::Installed
            | AppState::UpToDate
            | AppState::InstalledNotOffered
            | AppState::Failing => None,
        }
    }

    /// The word on the primary button.
    pub fn primary_label(&self, app: &AppId) -> Option<&'static str> {
        let entry = self.inventory.entry(app)?;
        match entry.state {
            AppState::Available => Some("INSTALL"),
            AppState::UpdateAvailable => Some("UPDATE"),
            AppState::Failing if entry.can_roll_back() => Some("ROLL BACK"),
            AppState::NothingSelected => Some("REPAIR"),
            _ => None,
        }
    }

    /// Whether a rollback button belongs on `app`'s detail view.
    pub fn can_roll_back(&self, app: &AppId) -> bool {
        self.inventory
            .entry(app)
            .is_some_and(AppEntry::can_roll_back)
    }

    /// Starts an operation, refusing to start a second one.
    ///
    /// One at a time, and the screen enforces it rather than relying on a
    /// person not pressing twice: the install transaction serialises on a
    /// per-app lock anyway, and a second press would produce a failure
    /// dialog for something the person plainly did not mean to ask twice.
    pub fn begin(&mut self, request: &Request) -> bool {
        // The rule is about *changing* things. A refresh or a notes fetch
        // reads, changes no install state, and is exactly what someone does
        // while waiting for a download — refusing those would be a spinner
        // that also disables the screen.
        let app = match request {
            Request::Install { app, .. } | Request::Rollback { app } => app.clone(),
            Request::Refresh | Request::Notes { .. } => return true,
        };
        if self.working.is_some() {
            return false;
        }
        self.failure = None;
        self.working = Some(Working {
            app,
            step: "STARTING".to_owned(),
            percent: None,
        });
        true
    }

    /// Updates the step shown for the operation in flight.
    pub fn report(&mut self, step: &str, percent: Option<u8>) {
        if let Some(working) = &mut self.working {
            working.step = step.to_owned();
            working.percent = percent;
        }
    }

    /// Folds a finished request back into the screen.
    pub fn apply(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Refreshed(inventory) => {
                self.inventory = *inventory;
                // A detail view whose app has vanished from the catalog and
                // from disk has nothing left to show.
                if let View::Detail(app) = &self.view
                    && self.inventory.entry(app).is_none()
                {
                    self.view = View::List;
                    self.notes = None;
                }
            }
            Outcome::Notes { app, text } => self.notes = Some((app, text)),
            Outcome::Installed { .. } | Outcome::RolledBack { .. } => {
                self.working = None;
                self.failure = None;
            }
            Outcome::Failed {
                message, advice, ..
            } => {
                self.working = None;
                self.failure = Some(Failure {
                    message,
                    advice: advice.to_owned(),
                });
            }
        }
    }

    /// The name to draw for an app, whatever the inventory could work out.
    pub fn name_of(&self, app: &AppId) -> DisplayName {
        self.inventory
            .entry(app)
            .map_or_else(|| DisplayName::from_lossy(app.leaf()), |e| e.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use paper_packages::inventory::Inventory;
    use paper_packages::launch::Ledger;
    use paper_packages::store::Layout;

    use super::{AppStoreScreen, Outcome, Request, View};

    /// An empty inventory over a store that exists but holds nothing.
    fn empty() -> Inventory {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let layout = Layout::new(dir.path());
        layout.ensure().expect("an empty store");
        Inventory::survey(&layout, None, &Ledger::new(layout.clone())).expect("an empty survey")
    }

    fn screen() -> AppStoreScreen {
        AppStoreScreen::new(empty())
    }

    #[test]
    fn an_empty_store_shows_an_empty_list_and_no_catalog() {
        let screen = screen();
        assert!(screen.rows().is_empty());
        assert_eq!(screen.view(), &View::List);
        assert_eq!(screen.catalog_summary(), "NO CATALOG");
        assert!(!screen.is_stale());
        assert!(screen.working().is_none());
        assert!(screen.failure().is_none());
    }

    #[test]
    fn opening_an_app_that_is_not_there_does_nothing() {
        let mut screen = screen();
        let app = "dev.calum.chess".parse().expect("a valid id");
        assert_eq!(screen.open(&app), None);
        // The view does not move to a detail page for an app with no row.
        assert_eq!(screen.view(), &View::List);
    }

    #[test]
    fn a_second_operation_cannot_start_while_one_is_in_flight() {
        let mut screen = screen();
        let app: paper_packages::AppId = "dev.calum.chess".parse().expect("a valid id");
        let request = Request::Install {
            app: app.clone(),
            version: "1.0.0".parse().expect("a valid version"),
        };

        assert!(screen.begin(&request));
        assert_eq!(screen.working().expect("working").app, app);
        assert!(!screen.begin(&request), "a second press must be refused");
        assert!(
            !screen.begin(&Request::Rollback { app: app.clone() }),
            "a different change must be refused too"
        );

        // Reads are not operations: someone waiting on a download can still
        // refresh the list and open release notes.
        assert!(screen.begin(&Request::Refresh));
        assert!(screen.begin(&Request::Notes {
            app,
            version: "1.0.0".parse().expect("a valid version"),
        }));
        assert!(
            screen.working().is_some(),
            "a read must not clear the operation in flight"
        );
    }

    #[test]
    fn progress_is_shown_and_cleared_by_the_outcome() {
        let mut screen = screen();
        let app: paper_packages::AppId = "dev.calum.chess".parse().expect("a valid id");
        screen.begin(&Request::Install {
            app: app.clone(),
            version: "1.0.0".parse().expect("a valid version"),
        });

        screen.report("DOWNLOADING", Some(42));
        let working = screen.working().expect("working");
        assert_eq!(working.step, "DOWNLOADING");
        assert_eq!(working.percent, Some(42));

        screen.apply(Outcome::Installed {
            app: app.clone(),
            version: "1.0.0".parse().expect("a valid version"),
        });
        assert!(screen.working().is_none());
        assert!(screen.failure().is_none());
    }

    #[test]
    fn a_failure_replaces_progress_and_carries_its_advice() {
        let mut screen = screen();
        let app: paper_packages::AppId = "dev.calum.chess".parse().expect("a valid id");
        screen.begin(&Request::Rollback { app: app.clone() });

        screen.apply(Outcome::Failed {
            app: Some(app),
            message: "the catalog is not available".to_owned(),
            advice: "Check the home network.",
        });

        assert!(screen.working().is_none(), "progress must stop");
        let failure = screen.failure().expect("a failure");
        assert_eq!(failure.advice, "Check the home network.");

        screen.dismiss_failure();
        assert!(screen.failure().is_none());
        // And the screen accepts work again afterwards.
        assert!(screen.begin(&Request::Refresh));
    }

    #[test]
    fn notes_are_only_shown_for_the_app_being_looked_at() {
        let mut screen = screen();
        let chess: paper_packages::AppId = "dev.calum.chess".parse().expect("a valid id");
        screen.apply(Outcome::Notes {
            app: chess,
            text: "Castling works now.".to_owned(),
        });
        // Still on the list, so nothing is shown.
        assert_eq!(screen.notes(), None);
    }

    #[test]
    fn a_refresh_closes_a_detail_view_whose_app_has_gone() {
        let mut screen = screen();
        screen.view = View::Detail("dev.calum.chess".parse().expect("a valid id"));
        screen.apply(Outcome::Refreshed(Box::new(empty())));
        assert_eq!(screen.view(), &View::List);
        assert_eq!(screen.notes(), None);
    }

    #[test]
    fn going_back_forgets_the_notes_it_was_showing() {
        let mut screen = screen();
        screen.view = View::Detail("dev.calum.chess".parse().expect("a valid id"));
        screen.apply(Outcome::Notes {
            app: "dev.calum.chess".parse().expect("a valid id"),
            text: "notes".to_owned(),
        });
        assert_eq!(screen.notes(), Some("notes"));
        screen.back();
        assert_eq!(screen.view(), &View::List);
        assert_eq!(screen.notes(), None);
    }
}

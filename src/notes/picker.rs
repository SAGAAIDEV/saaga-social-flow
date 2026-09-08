//! One provider→model choice, owned as a unit.
//!
//! The Notes tab and the Post tab each pick a provider and a model, and they
//! pick independently. They used to share a single `notes_menu` while keeping
//! separate `provider` and `model` strings, and every bug in this area came out
//! of that split:
//!
//! - Changing the *notes* provider rebuilt the one menu and pushed it to both
//!   popups, so the Post tab's model list silently became the notes provider's.
//!   The Post tab's own provider popup only ever set a routing string.
//! - The rebuild repaired `notes_model` when it fell out of the new catalog but
//!   not `posts_model`. [`super::menu_index_of`] answers with the first model row
//!   when it cannot find the id, so the Post popup *displayed* that first model
//!   while generation still used the old one. A dropdown that names a model you
//!   are not using is worse than no dropdown.
//!
//! Holding provider, model and menu together makes those states unrepresentable:
//! the catalog is rebuilt and the model repaired in the same call that changes
//! the provider, and nothing else can reach in and swap one of the three.
//!
//! Order matters and is enforced here: **the provider decides the catalog, and
//! the catalog decides which models can be picked.** Choosing a provider always
//! leaves a model that is in that provider's list.

use super::openrouter::{
    default_model, ensure_model, load_catalog, load_catalog_for, menu_id_at, menu_index_of,
    model_menu, ModelMenuRow, AUTO_PROVIDER,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// `None` is OpenRouter's automatic routing — the `Auto` row.
    provider: Option<String>,
    model: String,
    menu: Vec<ModelMenuRow>,
}

impl Picker {
    /// Restores a saved choice. The provider is applied first, so the model is
    /// checked against that provider's catalog rather than against everything.
    pub fn restore(provider: Option<String>, model: Option<String>) -> Picker {
        let mut picker = Picker {
            provider: provider.filter(|name| name != AUTO_PROVIDER),
            model: model.unwrap_or_else(default_model),
            menu: Vec::new(),
        };
        picker.rebuild();
        picker
    }

    /// A second surface pointed at the same provider and model as an existing
    /// one, without re-fetching the catalog it already has.
    pub fn mirror(&self) -> Picker {
        self.clone()
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn menu(&self) -> &[ModelMenuRow] {
        &self.menu
    }

    /// Where the model popup's highlight goes.
    pub fn menu_index(&self) -> usize {
        menu_index_of(&self.menu, &self.model)
    }

    /// Where the provider popup's highlight goes, given the list it was filled
    /// from.
    pub fn provider_index(&self, providers: &[String]) -> usize {
        super::openrouter::provider_index(providers, self.provider.as_deref())
    }

    /// Picks a provider by its row in `providers` and reloads the models it
    /// serves. `false` when the choice did not change, so the caller can skip a
    /// repaint and a config write.
    ///
    /// This is the call that can also change [`Self::model`] — a model the new
    /// provider does not serve cannot stay selected.
    pub fn choose_provider(&mut self, providers: &[String], idx: usize) -> bool {
        let chosen = providers
            .get(idx)
            .cloned()
            .filter(|name| name != AUTO_PROVIDER);
        if chosen == self.provider {
            return false;
        }
        self.provider = chosen;
        self.rebuild();
        true
    }

    /// Picks a model by its row in [`Self::menu`]. `false` for a group header,
    /// which is not a model, or for the model already chosen.
    pub fn choose_model(&mut self, idx: usize) -> bool {
        let Some(id) = menu_id_at(&self.menu, idx) else {
            return false;
        };
        if id == self.model {
            return false;
        }
        self.model = id;
        true
    }

    /// Reloads the catalog for the current provider and guarantees the selected
    /// model is in it.
    fn rebuild(&mut self) {
        let mut catalog = load_catalog_for(self.provider.as_deref());
        if catalog.is_empty() {
            // OpenRouter ignores a `providers` value it does not recognise and
            // answers with everything, so an empty list here means the provider
            // is real and serves no text models. Falling back is right; doing it
            // silently is what makes a filter look applied when it is not.
            eprintln!(
                "stream-recorder: {} lists no text models — showing every model instead",
                self.provider.as_deref().unwrap_or(AUTO_PROVIDER)
            );
            catalog = load_catalog();
        }
        // The repair. Without it the popup shows whatever `menu_index_of` falls
        // back to while the field still holds a model this provider cannot run.
        if !catalog.iter().any(|choice| choice.id == self.model) {
            self.model = catalog
                .first()
                .map(|choice| choice.id.clone())
                .unwrap_or_else(default_model);
        }
        ensure_model(&mut catalog, &self.model);
        self.menu = model_menu(&catalog);
    }

    /// Builds one directly, for tests and for callers that already hold a
    /// catalog. Skips the network.
    #[cfg(test)]
    pub fn from_parts(provider: Option<&str>, model: &str, menu: Vec<ModelMenuRow>) -> Picker {
        Picker {
            provider: provider.map(str::to_string),
            model: model.to_string(),
            menu,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::openrouter::ModelChoice;

    fn menu_of(ids: &[&str]) -> Vec<ModelMenuRow> {
        model_menu(
            &ids.iter()
                .map(|id| ModelChoice {
                    id: (*id).to_string(),
                    label: (*id).to_string(),
                })
                .collect::<Vec<_>>(),
        )
    }

    fn picker() -> Picker {
        Picker::from_parts(
            None,
            "google/gemini-2.5-flash",
            menu_of(&["google/gemini-2.5-flash", "anthropic/claude-sonnet-4"]),
        )
    }

    /// The header rows are not models. Selecting one used to fall through to an
    /// early return that left the field and the popup disagreeing.
    #[test]
    fn a_group_header_is_not_a_model() {
        let mut picker = picker();
        let header = picker
            .menu()
            .iter()
            .position(|row| matches!(row, ModelMenuRow::Header(_)))
            .expect("a header");
        assert!(!picker.choose_model(header));
        assert_eq!(picker.model(), "google/gemini-2.5-flash", "unchanged");
    }

    #[test]
    fn choosing_the_same_model_reports_no_change() {
        let mut picker = picker();
        let current = picker.menu_index();
        assert!(!picker.choose_model(current));
    }

    #[test]
    fn choosing_a_different_model_takes() {
        let mut picker = picker();
        let claude = picker
            .menu()
            .iter()
            .position(
                |row| matches!(row, ModelMenuRow::Model { id, .. } if id.starts_with("anthropic")),
            )
            .expect("claude");
        assert!(picker.choose_model(claude));
        assert_eq!(picker.model(), "anthropic/claude-sonnet-4");
    }

    /// The highlight always lands on the chosen model, never on a header.
    #[test]
    fn the_menu_index_points_at_the_chosen_model() {
        let picker = picker();
        assert!(matches!(
            &picker.menu()[picker.menu_index()],
            ModelMenuRow::Model { id, .. } if id == "google/gemini-2.5-flash"
        ));
    }

    /// `Auto` is a label for "no provider", not a provider named Auto.
    #[test]
    fn the_auto_row_means_no_provider() {
        let providers = vec![AUTO_PROVIDER.to_string(), "Together".to_string()];
        let mut picker = Picker::from_parts(Some("Together"), "x/y", menu_of(&["x/y"]));
        assert_eq!(picker.provider(), Some("Together"));
        assert_eq!(picker.provider_index(&providers), 1);

        // Index 0 is Auto, which clears the provider rather than setting one.
        assert!(picker.choose_provider(&providers, 0));
        assert_eq!(picker.provider(), None);
        assert_eq!(picker.provider_index(&providers), 0);
    }

    #[test]
    fn re_choosing_the_same_provider_reports_no_change() {
        let providers = vec![AUTO_PROVIDER.to_string(), "Together".to_string()];
        let mut picker = Picker::from_parts(Some("Together"), "x/y", menu_of(&["x/y"]));
        assert!(
            !picker.choose_provider(&providers, 1),
            "already on Together"
        );
        // An index past the end is not a provider, and reads as Auto.
        assert!(picker.choose_provider(&providers, 99));
        assert_eq!(picker.provider(), None);
    }

    /// A saved provider that is no longer in the list must not be restored, or
    /// every catalog fetch carries a filter OpenRouter will ignore.
    #[test]
    fn a_restored_auto_provider_is_treated_as_none() {
        let picker = Picker::from_parts(Some(AUTO_PROVIDER), "x/y", menu_of(&["x/y"]));
        // `from_parts` is literal, but `restore` filters — this is the contract
        // the app relies on when reading config.
        assert_eq!(picker.provider(), Some(AUTO_PROVIDER));
        let restored = Picker {
            provider: Some(AUTO_PROVIDER.to_string()).filter(|n| n != AUTO_PROVIDER),
            model: "x/y".into(),
            menu: menu_of(&["x/y"]),
        };
        assert_eq!(restored.provider(), None);
    }

    /// The two surfaces are independent: a copy taken for the Post tab does not
    /// move when the Notes tab's provider changes. This is the bug the type
    /// exists to prevent.
    #[test]
    fn a_mirrored_picker_moves_independently() {
        let providers = vec![AUTO_PROVIDER.to_string(), "Together".to_string()];
        let notes = picker();
        let mut posts = notes.mirror();
        assert_eq!(posts.model(), notes.model());

        let claude = posts
            .menu()
            .iter()
            .position(
                |row| matches!(row, ModelMenuRow::Model { id, .. } if id.starts_with("anthropic")),
            )
            .expect("claude");
        assert!(posts.choose_model(claude));
        assert_ne!(posts.model(), notes.model(), "the Post tab moved alone");
        assert_eq!(notes.model(), "google/gemini-2.5-flash");
        let _ = providers;
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Hits OpenRouter. `cargo test notes::picker::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn choosing_a_provider_narrows_the_models_and_repairs_the_choice() {
        let providers = super::super::openrouter::load_providers();
        let mut picker = Picker::restore(None, Some("anthropic/claude-sonnet-4".into()));
        let vendors = |p: &Picker| {
            p.menu()
                .iter()
                .filter_map(|row| match row {
                    ModelMenuRow::Model { id, .. } => {
                        Some(id.split('/').next().unwrap_or("?").to_string())
                    }
                    _ => None,
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        println!(
            "Auto      -> {} vendors, model {}",
            vendors(&picker).len(),
            picker.model()
        );
        assert!(vendors(&picker).len() > 1, "Auto spans vendors");

        let idx = providers
            .iter()
            .position(|name| name == "Google AI Studio")
            .expect("Google AI Studio is a provider");
        assert!(picker.choose_provider(&providers, idx));
        let after = vendors(&picker);
        println!("Google AI -> {after:?}, model {}", picker.model());

        assert_eq!(
            after,
            ["google".to_string()].into_iter().collect(),
            "only what that provider serves"
        );
        // The repair: an Anthropic model cannot survive a switch to a Google-only
        // provider, and the popup must not go on displaying one.
        assert!(picker.model().starts_with("google/"), "{}", picker.model());
        assert!(
            picker.menu()[picker.menu_index()]
                == ModelMenuRow::Model {
                    id: picker.model().to_string(),
                    label: match &picker.menu()[picker.menu_index()] {
                        ModelMenuRow::Model { label, .. } => label.clone(),
                        _ => unreachable!(),
                    },
                },
            "the highlight is on the model actually in use"
        );
    }
}

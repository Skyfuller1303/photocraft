//! The question File › Save As asks first: save to CraftCloud or on this computer.
//!
//! It is modelled on the "Save on your computer or to cloud documents" dialog of Creative Cloud
//! apps. The chooser only collects the answer; [`super::Runtime`] acts on it (the CraftCloud save
//! wizard, or the unchanged native Save As). "Don't show again" is stored in the CraftCloud
//! settings and can be undone with Preferences › File Handling › Ask where to save.

use egui::{Key, RichText, vec2};
use photocraft_doc::DocId;

use crate::FormatChoices;
use crate::theme::Tokens;

/// Size of one choice card.
const CARD: egui::Vec2 = vec2(236.0, 92.0);

/// Where the user wants to save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Cloud,
    Computer,
}

/// How the chooser was left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A card was chosen; `remember` is the state of "Don't show again".
    Choose {
        choice: Choice,
        remember: bool,
    },
    Cancel,
}

/// A Save As waiting for the answer. The native Save As continues from `choices` and `plain_save`
/// exactly as it would have without the chooser.
pub(crate) struct Chooser {
    pub(crate) doc: DocId,
    pub(crate) choices: FormatChoices,
    pub(crate) plain_save: bool,
    remember: bool,
    focused: bool,
}

impl Chooser {
    pub(crate) fn new(doc: DocId, choices: FormatChoices, plain_save: bool) -> Self {
        Self { doc, choices, plain_save, remember: false, focused: false }
    }
}

/// One card: the large choice button, with its one-line description underneath.
fn card(ui: &mut egui::Ui, primary: bool, title: &str, description: &str, focus: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    ui.vertical(|ui| {
        ui.set_width(CARD.x);
        let (fill, text) = if primary { (t.primary_bg, t.primary_text) } else { (t.field, t.text) };
        let button = egui::Button::new(RichText::new(tl!(title)).font(crate::theme::semibold(15.0)).color(text))
            .min_size(CARD)
            .fill(fill)
            .corner_radius(t.radius_sm + 2.0);
        let response = ui.add(button);
        if focus {
            response.request_focus();
        }
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(tl!(description)).color(t.text_dim).size(12.0)).wrap());
        response
    })
    .inner
}

/// Draws the chooser for one frame. Returns the outcome once the user has answered.
///
/// Keys: Enter chooses CraftCloud (unless Tab moved the focus to another control, which then takes
/// the Enter itself), Esc cancels, and Tab walks cloud, computer, the checkbox and Cancel.
pub(crate) fn show(ctx: &egui::Context, chooser: &mut Chooser) -> Option<Outcome> {
    let first = !std::mem::replace(&mut chooser.focused, true);
    let mut outcome = None;
    let remember = &mut chooser.remember;
    let modal = egui::Modal::new(egui::Id::unique("craftcloud-save-chooser")).show(ctx, |ui| {
        ui.set_max_width(CARD.x * 2.0 + 24.0);
        ui.label(RichText::new(tl!("Save As")).font(crate::theme::semibold(15.0)));
        ui.add_space(4.0);
        crate::widgets::hairline(ui);
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 24.0;
            if card(ui, true, "Save to CraftCloud", "Access from any device, share with your team", first).clicked() {
                outcome = Some(Choice::Cloud);
            }
            if card(ui, false, "Save on this computer", "Save a file on your computer", false).clicked() {
                outcome = Some(Choice::Computer);
            }
        });
        ui.add_space(10.0);
        crate::widgets::checkbox(ui, remember, "Don't show again");
        ui.add_space(8.0);
        if crate::widgets::secondary_button(ui, "Cancel", 84.0).clicked() {
            return Some(Outcome::Cancel);
        }
        None
    });
    let mut result = modal.inner;
    if result.is_none() {
        result = outcome.map(|choice| Outcome::Choose { choice, remember: chooser.remember });
    }
    if result.is_none() && modal.should_close() {
        result = Some(Outcome::Cancel);
    }
    // A focused control (the cloud card has the focus when the chooser opens, Tab moves it) takes
    // the Enter as its own click above; with nothing focused, Enter means CraftCloud.
    if result.is_none() && ctx.memory(|m| m.focused()).is_none() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter)) {
        result = Some(Outcome::Choose { choice: Choice::Cloud, remember: chooser.remember });
    }
    result
}

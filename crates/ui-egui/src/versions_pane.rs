//! The Version History pane and the banner shown while a previous version is open.

use egui::{Align, Stroke, Ui};
use serde_json::json;

use crate::theme::{Tokens, regular, semibold};
use crate::{WordApp, tl};

/// Draw the right hand history pane when File › Info › Version History or the title is clicked.
pub fn show(app: &mut WordApp, ui: &mut Ui) {
    if !app.ui.versions_pane {
        return;
    }
    let t = Tokens::get(ui.ctx());
    egui::Panel::right("versions_history")
        .default_size(330.0)
        .resizable(true)
        .frame(egui::Frame::NONE.fill(t.panel).inner_margin(12).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Version History")).font(semibold(16.0)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if crate::widgets::icon_button(ui, "close", tl!("Close")).clicked() {
                        app.ui.versions_pane = false;
                    }
                });
            });
            ui.add_space(8.0);
            let versions = app.session.version_infos();
            if versions.is_empty() {
                let legacy = app.session.versions.clone();
                if legacy.is_empty() {
                    ui.label(egui::RichText::new(tl!("No versions yet.")).font(regular(12.5)).color(t.text_dim));
                } else {
                    for (index, (label, date, _)) in legacy.iter().enumerate().rev() {
                        legacy_row(app, ui, index, label, date, t);
                    }
                }
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                let mut day = String::new();
                for info in versions {
                    let current_day = info.saved_at.get(..10).unwrap_or(&info.saved_at).to_string();
                    if current_day != day {
                        day = current_day.clone();
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(current_day).font(semibold(12.5)).color(t.text));
                    }
                    ui.group(|ui| {
                        ui.label(egui::RichText::new(info.saved_at.clone()).font(regular(12.0)).color(t.text));
                        ui.label(egui::RichText::new(info.author.clone()).font(regular(11.5)).color(t.text_dim));
                        ui.horizontal(|ui| {
                            let id = info.id.clone();
                            if ui.button(tl!("Open version")).clicked() {
                                let _ = app.run("file.versions", json!({"open": id}));
                            }
                            if ui.button(tl!("Restore")).clicked() {
                                let _ = app.run("file.versions", json!({"restore": info.id.clone()}));
                            }
                        });
                    });
                }
            });
        });
}

fn legacy_row(app: &mut WordApp, ui: &mut Ui, index: usize, label: &str, date: &str, t: Tokens) {
    ui.group(|ui| {
        ui.label(egui::RichText::new(date).font(regular(12.0)).color(t.text));
        ui.label(egui::RichText::new(label).font(regular(11.5)).color(t.text_dim));
        if ui.button(tl!("Restore")).clicked() {
            let _ = app.run("file.versions", json!({"restore": index}));
        }
    });
}

/// The strip Word shows above a document opened from Version History.
pub fn previous_bar(app: &mut WordApp, ui: &mut Ui) {
    let Some(info) = app.session.previous_version().cloned() else { return };
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("previous_version_bar")
        .exact_size(30.0)
        .frame(egui::Frame::NONE.fill(t.selection).inner_margin(egui::Margin::symmetric(12, 5)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Previous version")).font(semibold(12.5)).color(t.text));
                ui.label(egui::RichText::new(info.saved_at).font(regular(12.0)).color(t.text_dim));
                if ui.button(tl!("Restore")).clicked() {
                    let _ = app.run("file.versions", json!({"restore": info.id}));
                }
                if ui.button(tl!("Save a copy")).clicked() {
                    let _ = app.run("file.saveAs", json!({}));
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn app() -> WordApp {
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), crate::Services::default());
        app.session.versions.push(("First save".into(), "2026-10-16T10:00:00Z".into(), wordcraft_doc::Document::from_text("old")));
        app.ui.versions_pane = true;
        app
    }

    #[test]
    fn panel_lists_a_version_and_restore_runs_the_command() {
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app(),
        );
        harness.run_steps(6);
        assert!(harness.query_by_label("Restore").is_some());
        harness.get_by_label("Restore").click();
        harness.run_steps(2);
        assert_eq!(harness.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body), "old");
    }
}

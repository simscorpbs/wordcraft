//! The Version History pane and the bar shown while a previous version is open.
//!
//! As in Word with a document in OneDrive, the pane lists the versions newest first, grouped by
//! day, each with its time and author and an Open version link. An opened version is read only
//! (the engine refuses edits, `cmd::version_view`): the bar above it offers Compare, Restore,
//! Save a copy and Close, and its row in the pane is highlighted. WordCraft shows the previous
//! version in the document's own window instead of a second one, so closing it (or New, Open,
//! Close and quitting) first brings the document back with its unsaved changes.

use egui::{Align, CornerRadius, Sense, Stroke, Ui};
use serde_json::{Value, json};
use wordcraft_engine::cmd::version_view::READ_ONLY;
use wordcraft_engine::versions::{VersionInfo, local_timestamp};

use crate::WordApp;
use crate::theme::{Tokens, medium, regular, semibold};

/// A local date and time as [`local_timestamp`] writes it (`YYYY-MM-DD HH:MM`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

impl LocalTime {
    pub(crate) fn parse(local: &str) -> Option<Self> {
        let num = |range: std::ops::Range<usize>| local.get(range).and_then(|s| s.parse::<u32>().ok());
        let year = local.get(0..4).and_then(|s| s.parse::<i64>().ok())?;
        let (month, day, hour, minute) = (num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?);
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
            return None;
        }
        Some(Self { year, month, day, hour, minute })
    }

    /// Days since 1970-01-01 (Howard Hinnant's algorithm), to tell today from yesterday.
    fn days(self) -> i64 {
        let y = if self.month <= 2 { self.year - 1 } else { self.year };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn format(self, picture: &str, lang: &str) -> String {
        let month = usize::try_from(self.month).unwrap_or(1);
        wordcraft_engine::cmd::insert::format_date_parts_in(picture, Some(lang), self.year, month, self.day, (self.hour, self.minute, 0), self.days())
    }
}

/// The interface language's primary subtag (`pt` for Brazilian Portuguese).
fn language() -> &'static str {
    let code = crate::i18n::current().code();
    code.split(['-', '_']).next().unwrap_or(code)
}

/// A day heading: Today, Yesterday, or the date written the way the interface language writes it
/// (English and Portuguese with month names; other languages as an ISO date, never wrong).
pub(crate) fn day_heading(day: LocalTime, today: LocalTime) -> String {
    match today.days() - day.days() {
        0 => tl!("Today").to_string(),
        1 => tl!("Yesterday").to_string(),
        _ => match language() {
            "en" => day.format("MMMM d, yyyy", "en"),
            "pt" => day.format("d 'de' MMMM 'de' yyyy", "pt"),
            _ => day.format("yyyy-MM-dd", "en"),
        },
    }
}

/// A version's time of day in the interface language: `3:05 PM` in English, `15:05` elsewhere.
pub(crate) fn time_of_day(time: LocalTime) -> String {
    if language() == "en" { time.format("h:mm am/pm", "en") } else { time.format("HH:mm", "en") }
}

/// A version's date and time, for the bar and tooltips.
pub(crate) fn date_and_time(saved_at: &str) -> String {
    let local = local_timestamp(saved_at);
    let Some(time) = LocalTime::parse(&local) else { return local };
    let today = LocalTime::parse(&local_timestamp(&wordcraft_engine::cmd::now_iso())).unwrap_or(time);
    format!("{}, {}", day_heading(time, today), time_of_day(time))
}

/// Open or restore a version: AutoSave writes the document first, so its state up to now is a
/// version of its own in the history before it is replaced, as Word saves the current version
/// before restoring. Restoring from the previous version bar first brings the document back, so
/// its own unsaved changes are saved the same way. `Some` stops the command (a save conflict).
pub(crate) fn before_user_command(app: &mut WordApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != "file.versions" {
        return None;
    }
    let restore = params.get("restore").is_some_and(Value::is_string);
    if !restore && params.get("open").is_none() {
        return None;
    }
    if restore && app.session.previous_version().is_some() {
        let _ = close_previous_version(app);
    }
    if app.session.previous_version().is_none() && matches!(app.autosave_before_discard(), crate::AutoSaveBeforeDiscard::Conflict) {
        return Some(Ok(json!({"pending": "saveConflict"})));
    }
    None
}

/// Close the previous version and go back to the document, which AutoSave covers again as it did.
pub(crate) fn close_previous_version(app: &mut WordApp) -> Result<Value, String> {
    app.execute("file.closeVersion", json!({}))
}

/// After a command in [`WordApp::execute`]: keep the bar's state and AutoSave in step.
pub(crate) fn after_command(app: &mut WordApp, id: &str, params: &Value, r: &Result<Value, String>) {
    let Ok(v) = r else { return };
    match id {
        "file.closeVersion" => {
            app.autosave_path = app.version_autosave_path.take();
            app.version_compared = false;
        }
        "file.compareVersion" => app.version_compared = true,
        "file.versions" if params.get("open").is_some() || params.get("restore").is_some() => app.version_compared = false,
        // Save a copy: the previous version is now a file of its own, and the window goes back
        // to the document, as the copy's window would close in Word once it is saved.
        "file.save" | "file.saveAs"
            if app.session.previous_version().is_some() && app.session.path.is_some() && v.get("saved").and_then(Value::as_bool) == Some(true) =>
        {
            let copy = app.session.path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            if close_previous_version(app).is_ok() {
                app.status(crate::i18n::fmt(tl!("Saved a copy of the previous version as {path}"), &[("path", &copy)]));
            }
        }
        _ => {}
    }
}

/// The status line for a failed command: an edit refused in a previous version says why in the
/// interface language.
pub(crate) fn status_text(error: &str) -> String {
    if error.contains(READ_ONLY) { tl!(READ_ONLY).to_string() } else { error.to_string() }
}

/// Why the pane has no versions to list.
fn empty_message(app: &WordApp) -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "Version history is kept by the desktop app."
    } else if app.session.path.is_none() && app.session.previous_version().is_none() {
        "Save the document to start its version history."
    } else {
        "No versions yet."
    }
}

/// Draw the right hand history pane when File › Info › Version History or the title is clicked.
pub fn show(app: &mut WordApp, ui: &mut Ui) {
    if !app.ui.versions_pane {
        return;
    }
    let t = Tokens::get(ui.ctx());
    egui::Panel::right("versions_history")
        .default_size(300.0)
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
                    ui.label(egui::RichText::new(tl!(empty_message(app))).font(regular(12.5)).color(t.text_dim));
                } else {
                    for (index, (label, date, _)) in legacy.iter().enumerate().rev() {
                        legacy_row(app, ui, index, label, date, t);
                    }
                }
                return;
            }
            let viewing = app.session.previous_version().map(|v| v.id.clone());
            let today = LocalTime::parse(&local_timestamp(&wordcraft_engine::cmd::now_iso()));
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                let mut day: Option<(i64, u32, u32)> = None;
                for (n, info) in versions.iter().enumerate() {
                    let local = local_timestamp(&info.saved_at);
                    let time = LocalTime::parse(&local);
                    let this_day = time.map(|t| (t.year, t.month, t.day));
                    if n == 0 || this_day != day {
                        day = this_day;
                        ui.add_space(8.0);
                        let heading = match (time, today) {
                            (Some(time), Some(today)) => day_heading(time, today),
                            _ => local.get(..10).unwrap_or(&local).to_string(),
                        };
                        ui.label(egui::RichText::new(heading).font(semibold(12.5)).color(t.text));
                        ui.add_space(2.0);
                    }
                    let shown = time.map(time_of_day).unwrap_or_else(|| local.clone());
                    version_row(app, ui, info, &shown, viewing.as_deref() == Some(info.id.as_str()), t);
                }
            });
        });
}

/// The id of a version's row in the pane.
pub(crate) fn row_id(version: &str) -> egui::Id {
    egui::Id::new(("version_row", version))
}

/// One version: its time and author, and Open version. The version being viewed is highlighted
/// and has no link (it is already open). Right-click offers Open version and Restore.
fn version_row(app: &mut WordApp, ui: &mut Ui, info: &VersionInfo, time: &str, viewing: bool, t: Tokens) {
    let fill = if viewing { t.selection } else { egui::Color32::TRANSPARENT };
    // The row senses clicks under its contents (the link keeps its own), for its menu.
    let row = egui::UiBuilder::new().id_salt(row_id(&info.id)).sense(Sense::click());
    let inner = ui.scope_builder(row, |ui| {
        egui::Frame::NONE.fill(fill).corner_radius(CornerRadius::same(4)).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(time).font(medium(12.5)).color(t.text));
            ui.label(egui::RichText::new(info.author.clone()).font(regular(11.5)).color(t.text_dim));
            if !viewing {
                let link = egui::Button::new(egui::RichText::new(tl!("Open version")).font(regular(12.0)).color(t.accent_text)).frame(false);
                if ui.add(link).clicked() {
                    let _ = app.run("file.versions", json!({"open": info.id.clone()}));
                }
            }
        });
    });
    let response = inner.response.on_hover_text(date_and_time(&info.saved_at));
    response.context_menu(|ui| {
        if !viewing && ui.button(tl!("Open version")).clicked() {
            let _ = app.run("file.versions", json!({"open": info.id.clone()}));
            ui.close();
        }
        if ui.button(tl!("Restore")).clicked() {
            let _ = app.run("file.versions", json!({"restore": info.id.clone()}));
            ui.close();
        }
    });
    ui.add_space(2.0);
}

fn legacy_row(app: &mut WordApp, ui: &mut Ui, index: usize, label: &str, date: &str, t: Tokens) {
    ui.group(|ui| {
        ui.label(egui::RichText::new(local_timestamp(date)).font(regular(12.0)).color(t.text));
        ui.label(egui::RichText::new(label).font(regular(11.5)).color(t.text_dim));
        if ui.button(tl!("Restore")).clicked() {
            let _ = app.run("file.versions", json!({"restore": index}));
        }
    });
}

/// The bar Word shows above a document opened from Version History: which version this is, and
/// Compare, Restore, Save a copy and Close.
pub fn previous_bar(app: &mut WordApp, ui: &mut Ui) {
    let Some(info) = app.session.previous_version().cloned() else { return };
    let t = Tokens::get(ui.ctx());
    let when = date_and_time(&info.saved_at);
    egui::Panel::top("previous_version_bar")
        .exact_size(36.0)
        .frame(egui::Frame::NONE.fill(t.selection).inner_margin(egui::Margin::symmetric(12, 6)).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(egui::RichText::new(tl!("Previous version")).font(semibold(12.5)).color(t.text));
                ui.label(egui::RichText::new(format!("{when} · {}", info.author)).font(regular(12.0)).color(t.text_dim));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(tl!("Close")).on_hover_text(tl!("Back to the current version")).clicked() {
                        let _ = app.run("file.closeVersion", json!({}));
                    }
                    if ui.button(tl!("Save a copy")).clicked() {
                        let _ = app.run("file.saveAs", json!({}));
                    }
                    if ui.add(crate::widgets::primary_button(ui, tl!("Restore"))).clicked() {
                        let _ = app.run("file.versions", json!({"restore": info.id.clone()}));
                    }
                    let compare = egui::Button::new(tl!("Compare")).selected(app.version_compared);
                    let tip = if app.version_compared { tl!("Show the previous version again") } else { tl!("Show what changed since this version") };
                    if ui.add(compare).on_hover_text(tip).clicked() {
                        let _ = if app.version_compared {
                            app.run("file.versions", json!({"open": info.id.clone()}))
                        } else {
                            app.run("file.compareVersion", json!({}))
                        };
                    }
                });
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
        app
    }

    fn harness(app: WordApp) -> egui_kittest::Harness<'static, WordApp> {
        egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 760.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        )
    }

    fn text(app: &WordApp) -> String {
        app.session.doc.plain_text(wordcraft_doc::StoryRef::Body)
    }

    #[test]
    fn panel_lists_a_version_and_restore_runs_the_command() {
        let mut app = app();
        assert!(app.run("file.versions", json!({})).is_ok(), "opening Version History succeeds");
        let mut harness = harness(app);
        harness.run_steps(6);
        assert!(harness.query_by_label("Restore").is_some());
        harness.get_by_label("Restore").click();
        harness.run_steps(2);
        assert_eq!(text(harness.state()), "old");
    }

    #[test]
    fn days_are_today_yesterday_or_a_date() {
        let at = |s: &str| LocalTime::parse(s).unwrap();
        let today = at("2026-03-01 09:30");
        crate::i18n::set_current(crate::i18n::Lang::EN);
        assert_eq!(day_heading(at("2026-03-01 00:00"), today), "Today");
        assert_eq!(day_heading(at("2026-02-28 23:59"), today), "Yesterday", "across a month end");
        assert_eq!(day_heading(at("2026-02-27 08:00"), today), "February 27, 2026");
        assert_eq!(time_of_day(at("2026-02-27 15:05")), "3:05 PM");
        assert_eq!(time_of_day(at("2026-02-27 00:07")), "12:07 AM");
        assert_eq!(day_heading(at("2025-12-31 08:00"), at("2026-01-01 08:00")), "Yesterday", "across a year end");
        let pt = crate::i18n::Lang::all().find(|l| l.code() == "pt-br").unwrap();
        crate::i18n::set_current(pt);
        assert_eq!(day_heading(at("2026-03-01 00:00"), today), "Hoje");
        assert_eq!(day_heading(at("2026-02-28 00:00"), today), "Ontem");
        assert_eq!(day_heading(at("2026-02-27 08:00"), today), "27 de fevereiro de 2026");
        assert_eq!(time_of_day(at("2026-02-27 15:05")), "15:05");
        crate::i18n::set_current(crate::i18n::Lang::EN);
        assert!(LocalTime::parse("2026-13-01 00:00").is_none());
        assert!(LocalTime::parse("garbage").is_none());
        assert!(LocalTime::parse("").is_none());
    }

    /// A WordApp on a saved document with a local history: "First draft" was saved as a
    /// version, then the document was edited to "Second draft" (saved too) and typed into.
    #[cfg(not(target_arch = "wasm32"))]
    fn with_history(name: &str) -> (WordApp, std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("wordcraft-versions-pane-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("First draft"));
        session.enable_versions(dir.join("versions"), std::sync::Arc::new(|| {})).unwrap();
        let mut app = WordApp::new(session, crate::Services::default());
        let file = dir.join("Notes.docx");
        app.run("file.save", json!({"path": file.to_string_lossy()})).unwrap();
        let first = wait_for_versions(&mut app, 1).first().map(|v| v.id.clone()).unwrap();
        app.run("document.setText", json!({"text": "Second draft"})).unwrap();
        app.run("file.save", json!({})).unwrap();
        wait_for_versions(&mut app, 2);
        app.run("caret.docEnd", json!({})).unwrap();
        app.run("text.insert", json!({"text": " unsaved"})).unwrap();
        (app, dir, first)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn wait_for_versions(app: &mut WordApp, n: usize) -> Vec<VersionInfo> {
        for _ in 0..1000 {
            let list = app.session.version_infos();
            if list.len() >= n {
                return list;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the history never reached {n} versions");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_opened_version_is_read_only_highlighted_and_closes_back_to_the_document() {
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let (mut app, dir, first) = with_history("open");
        assert!(app.session.dirty);
        app.run("file.versions", json!({})).unwrap();
        let mut h = harness(app);
        h.run_steps(6);
        assert!(h.query_all_by_label("Today").next().is_some(), "versions are grouped under Today");
        assert_eq!(h.query_all_by_label("Open version").count(), 2, "every row can be opened");
        // The rows are in the history's order (saves in the same second keep the order they have).
        let row = h.state_mut().session.version_infos().iter().position(|v| v.id == first).unwrap();
        h.query_all_by_label("Open version").nth(row).unwrap().click();
        h.run_steps(2);
        // AutoSave wrote the unsaved edit before the version opened: it is a version too.
        let list = wait_for_versions(h.state_mut(), 3);
        assert_eq!(list.len(), 3);
        h.run_steps(4);
        assert_eq!(text(h.state()), "First draft");
        assert_eq!(h.state().session.previous_version().map(|v| v.id.clone()), Some(first.clone()));
        assert!(h.query_by_label("Previous version").is_some(), "the bar names the version");
        assert!(h.query_by_label("Compare").is_some() && h.query_by_label("Save a copy").is_some());
        assert_eq!(h.query_all_by_label("Open version").count(), 2, "the open version's row has no link: it is highlighted");
        assert!(!h.state().session.dirty, "the previous version has no changes of its own");
        // Typing is refused, with the reason on the status line.
        let refused = h.state_mut().run("text.insert", json!({"text": "x"}));
        assert!(refused.is_err());
        assert_eq!(text(h.state()), "First draft");
        assert_eq!(h.state().status_msg.as_ref().map(|(m, _)| m.as_str()), Some(READ_ONLY));
        // Compare shows the changes as tracked changes, and pressing it again shows the version.
        h.get_by_label("Compare").click();
        h.run_steps(3);
        assert!(h.state().version_compared);
        assert!(!h.state().session.doc.revisions.is_empty());
        h.get_by_label("Compare").click();
        h.run_steps(3);
        assert!(!h.state().version_compared);
        assert_eq!(text(h.state()), "First draft");
        // Close goes back to the document as it was, unsaved changes and all.
        h.get_by_label("Close").click();
        h.run_steps(3);
        assert!(h.state().session.previous_version().is_none());
        assert_eq!(text(h.state()), "Second draft unsaved");
        assert!(!h.state().session.dirty, "AutoSave wrote the edit before the version opened");
        assert!(h.state().saved_here(), "and AutoSave still covers the file");
        assert!(h.query_by_label("Previous version").is_none());
        drop(h);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn restore_from_the_bar_makes_the_version_current_and_undo_takes_it_back() {
        let (mut app, dir, first) = with_history("restore");
        app.run("file.versions", json!({"open": first})).unwrap();
        assert_eq!(text(&app), "First draft");
        app.run("file.versions", json!({"restore": first})).unwrap();
        assert!(app.session.previous_version().is_none());
        assert_eq!(text(&app), "First draft");
        assert_eq!(app.session.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()), Some("Notes.docx".into()));
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(text(&app), "Second draft unsaved", "the document before Restore, with its unsaved edit");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Closing the document, opening another or quitting while a previous version is shown never
    /// loses the document's unsaved changes: Close closes only the version, and the others ask.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn close_new_and_quit_go_back_to_the_document_first() {
        let (mut app, dir, first) = with_history("discard");
        // AutoSave off first: the unsaved changes stay unsaved, so they can be asked about.
        app.autosave = false;
        app.run("file.versions", json!({"open": first.clone()})).unwrap();
        app.run("file.close", json!({})).unwrap();
        assert!(app.has_document());
        assert_eq!(text(&app), "Second draft unsaved", "Close closed only the previous version");

        // New asks about the document's unsaved changes.
        app.run("file.versions", json!({"open": first.clone()})).unwrap();
        let r = app.run("file.new", json!({})).unwrap();
        assert_eq!(r["pending"], json!("saveChanges"), "the document's unsaved changes are asked about");
        assert_eq!(text(&app), "Second draft unsaved");
        app.dialog = None;

        app.run("file.versions", json!({"open": first.clone()})).unwrap();
        assert!(!app.close_requested(), "quitting asks about the document's changes");
        assert!(app.session.previous_version().is_none());
        assert_eq!(text(&app), "Second draft unsaved");
        app.dialog = None;

        // With AutoSave on, New writes the document's changes to its file first.
        app.autosave = true;
        app.run("file.versions", json!({"open": first})).unwrap();
        app.run("file.new", json!({})).unwrap();
        let saved = wordcraft_engine::io::open_path(&dir.join("Notes.docx")).unwrap();
        assert_eq!(saved.plain_text(wordcraft_doc::StoryRef::Body), "Second draft unsaved");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn save_a_copy_keeps_the_version_and_goes_back_to_the_document() {
        let (mut app, dir, first) = with_history("copy");
        app.run("file.versions", json!({"open": first})).unwrap();
        let copy = dir.join("Notes copy.docx");
        app.run("file.saveAs", json!({"path": copy.to_string_lossy()})).unwrap();
        assert!(copy.exists());
        assert!(app.session.previous_version().is_none());
        assert_eq!(text(&app), "Second draft unsaved");
        assert_eq!(app.session.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()), Some("Notes.docx".into()));
        let reopened = wordcraft_engine::io::open_path(&copy).unwrap();
        assert_eq!(reopened.plain_text(wordcraft_doc::StoryRef::Body), "First draft");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_pane_says_why_it_is_empty() {
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), crate::Services::default());
        app.run("file.versions", json!({})).unwrap();
        let mut h = harness(app);
        h.run_steps(4);
        assert!(h.query_by_label("Save the document to start its version history.").is_some());
    }
}
